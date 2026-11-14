//! Cartridge: install the BIOS files you own onto a Batocera device.
//!
//! How Batocera works (see README):
//!  * BIOS files live in /userdata/bios/ on the Batocera device.
//!  * Running `batocera-systems` over SSH lists what is missing:
//!        > psx
//!        MISSING 8dd7d5296a650fac7319bce665a6a53c bios/scph5500.bin
//!  * "UNTESTED" means the file is there but its MD5 is wrong.
//!
//! Cartridge: 1) connects over SSH  2) asks Batocera what is missing
//!            3) matches files from a folder YOU choose  4) uploads them over SFTP.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, vec2, Color32, RichText, Stroke, Ui};
use std::collections::BTreeMap;
use md5::{Digest, Md5};
use ssh2::Session;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

// ---------- Data types ----------

#[derive(Clone)]
struct Creds {
    host: String,
    user: String,
    password: String,
}

/// One line of `batocera-systems` output.
#[derive(Clone)]
struct Entry {
    system: String,
    missing: bool,       // true = MISSING, false = UNTESTED (wrong version)
    md5s: Vec<String>,   // accepted checksums (empty = any file with that name)
    file: String,        // e.g. "bios/scph5500.bin"
}

#[derive(Clone)]
struct LocalFile {
    path: PathBuf,
    name_lower: String,
    md5: String,
}

#[derive(Clone)]
struct PlanItem {
    local: PathBuf,
    remote_rel: String, // e.g. "bios/scph5500.bin"
    system: String,
}

enum Msg {
    Connected(Vec<Entry>),
    Local(Vec<LocalFile>),
    Installed { ok: Vec<String>, failed: Vec<String>, remaining: Vec<Entry> },
    Progress(usize, usize, String),
    Err(String),
}

#[derive(PartialEq, Clone, Copy)]
enum Step {
    Connect,
    Pick,
    Install,
}

// ---------- SSH helpers ----------

fn connect(c: &Creds) -> Result<Session, String> {
    let addr = format!("{}:22", c.host.trim());
    let sock = addr
        .to_socket_addrs()
        .map_err(|e| format!("Bad address: {e}"))?
        .next()
        .ok_or("Could not resolve that address")?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(8))
        .map_err(|e| format!("Could not reach {addr}: {e}. Is Batocera on and on the same network?"))?;
    let mut sess = Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|e| format!("SSH handshake failed: {e}"))?;
    sess.userauth_password(&c.user, &c.password)
        .map_err(|_| "Login failed. Batocera's default is user 'root', password 'linux'.".to_string())?;
    Ok(sess)
}

fn run(sess: &Session, cmd: &str) -> Result<String, String> {
    let mut ch = sess.channel_session().map_err(|e| e.to_string())?;
    ch.exec(cmd).map_err(|e| e.to_string())?;
    let mut out = String::new();
    ch.read_to_string(&mut out).map_err(|e| e.to_string())?;
    ch.wait_close().ok();
    Ok(out)
}

fn fetch_missing(sess: &Session) -> Result<Vec<Entry>, String> {
    let out = run(sess, "batocera-systems")?;
    Ok(parse_missing(&out))
}

/// Parse the output of `batocera-systems`.
fn parse_missing(text: &str) -> Vec<Entry> {
    let mut out = vec![];
    let mut system = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(s) = line.strip_prefix("> ") {
            system = s.to_string();
            continue;
        }
        let Some((status, rest)) = line.split_once(' ') else { continue };
        if status != "MISSING" && status != "UNTESTED" {
            continue;
        }
        let mut rest = rest;
        let mut md5s = vec![];
        if let Some(r) = rest.strip_prefix("- ") {
            rest = r; // no checksum known
        } else {
            // one or more 32-char hashes joined by " or "
            while rest.len() > 32
                && rest.is_char_boundary(32)
                && rest[..32].chars().all(|c| c.is_ascii_hexdigit())
            {
                md5s.push(rest[..32].to_lowercase());
                rest = &rest[32..];
                if let Some(r) = rest.strip_prefix(" or ") {
                    rest = r;
                } else {
                    rest = rest.trim_start();
                    break;
                }
            }
        }
        out.push(Entry { system: system.clone(), missing: status == "MISSING", md5s, file: rest.to_string() });
    }
    out
}

// ---------- Local scanning & matching ----------

fn scan_folder(dir: &Path) -> Vec<LocalFile> {
    let mut files = vec![];
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if !e.file_type().is_file() {
            continue;
        }
        if e.metadata().map(|m| m.len() > 300_000_000).unwrap_or(true) {
            continue; // BIOS files are small; skip huge files
        }
        let Ok(mut f) = std::fs::File::open(e.path()) else { continue };
        let mut hasher = Md5::new();
        let mut buf = vec![0u8; 65536];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(_) => break,
            }
        }
        files.push(LocalFile {
            path: e.path().to_path_buf(),
            name_lower: e.file_name().to_string_lossy().to_lowercase(),
            md5: format!("{:x}", hasher.finalize()),
        });
    }
    files
}

/// Decide which local file goes to which Batocera path.
/// 1) a file whose MD5 matches is used even if it has a different name
///    (it gets uploaded under the name Batocera expects)
/// 2) otherwise a file with the right name is used when Batocera has no checksum for it
///    (e.g. arcade .zip files).
fn make_plan(entries: &[Entry], local: &[LocalFile]) -> Vec<PlanItem> {
    let mut plan: Vec<PlanItem> = vec![];
    for e in entries {
        let base = Path::new(&e.file)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let hit = local
            .iter()
            .find(|l| e.md5s.contains(&l.md5))
            .or_else(|| local.iter().find(|l| e.md5s.is_empty() && l.name_lower == base));
        if let Some(l) = hit {
            if !plan.iter().any(|p| p.remote_rel == e.file) {
                plan.push(PlanItem { local: l.path.clone(), remote_rel: e.file.clone(), system: e.system.clone() });
            }
        }
    }
    plan
}

// ---------- Uploading ----------

fn upload(sess: &Session, plan: &[PlanItem], progress: &mut dyn FnMut(usize, usize, &str)) -> (Vec<String>, Vec<String>) {
    let (mut ok, mut failed) = (vec![], vec![]);
    let sftp = match sess.sftp() {
        Ok(s) => s,
        Err(e) => return (ok, plan.iter().map(|p| format!("{} ({e})", p.remote_rel)).collect()),
    };
    for (i, item) in plan.iter().enumerate() {
        progress(i, plan.len(), &item.remote_rel);
        // Safety: only plain relative paths under /userdata
        let rel = Path::new(&item.remote_rel);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            failed.push(format!("{} (unsafe path)", item.remote_rel));
            continue;
        }
        let remote = Path::new("/userdata").join(rel);
        // create folders one by one (errors ignored: they may already exist)
        let mut dir = PathBuf::from("/userdata");
        if let Some(parent) = rel.parent() {
            for comp in parent.components() {
                dir.push(comp);
                let _ = sftp.mkdir(&dir, 0o755);
            }
        }
        let result = (|| -> Result<(), String> {
            let data = std::fs::read(&item.local).map_err(|e| e.to_string())?;
            let mut rf = sftp.create(&remote).map_err(|e| e.to_string())?;
            rf.write_all(&data).map_err(|e| e.to_string())?;
            Ok(())
        })();
        match result {
            Ok(()) => ok.push(item.remote_rel.clone()),
            Err(e) => failed.push(format!("{} ({e})", item.remote_rel)),
        }
    }
    (ok, failed)
}

// ---------- Look & feel ----------

const BG: Color32 = Color32::from_rgb(18, 20, 26);
const CARD: Color32 = Color32::from_rgb(28, 31, 40);
const CARD2: Color32 = Color32::from_rgb(38, 42, 54);
const BORDER: Color32 = Color32::from_rgb(58, 63, 80);
const TEXT: Color32 = Color32::from_rgb(232, 234, 240);
const MUTED: Color32 = Color32::from_rgb(140, 147, 165);
const ACCENT: Color32 = Color32::from_rgb(255, 138, 61);
const GREEN: Color32 = Color32::from_rgb(76, 217, 130);
const RED: Color32 = Color32::from_rgb(255, 99, 107);
const YELLOW: Color32 = Color32::from_rgb(255, 205, 86);

fn apply_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.5);
    v.extreme_bg_color = Color32::from_rgb(14, 16, 21);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.rounding = egui::Rounding::same(8.0);
    }
    v.widgets.inactive.weak_bg_fill = CARD2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 57, 73);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    ctx.set_visuals(v);

    let mut s = (*ctx.style()).clone();
    s.spacing.item_spacing = vec2(10.0, 10.0);
    s.spacing.button_padding = vec2(16.0, 9.0);
    s.spacing.interact_size.y = 34.0;
    use egui::{FontFamily::Proportional, FontId, TextStyle::*};
    s.text_styles.insert(Heading, FontId::new(24.0, Proportional));
    s.text_styles.insert(Body, FontId::new(15.0, Proportional));
    s.text_styles.insert(Button, FontId::new(15.0, Proportional));
    s.text_styles.insert(Small, FontId::new(12.5, Proportional));
    ctx.set_style(s);
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(12.0)
        .inner_margin(egui::Margin::same(18.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn primary(ui: &mut Ui, enabled: bool, text: &str) -> bool {
    let b = egui::Button::new(RichText::new(text).strong().color(Color32::from_rgb(20, 20, 24)))
        .fill(ACCENT)
        .rounding(8.0)
        .min_size(vec2(0.0, 38.0));
    ui.add_enabled(enabled, b).clicked()
}

fn badge(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.18))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).small().strong());
        });
}

fn stat(ui: &mut Ui, n: usize, label: &str, color: Color32) {
    egui::Frame::none()
        .fill(CARD2)
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(20.0, 10.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(n.to_string()).size(28.0).strong().color(color));
                ui.label(RichText::new(label).color(MUTED).small());
            });
        });
}

fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 32.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, ACCENT);
    p.rect_filled(egui::Rect::from_min_size(rect.min + vec2(5.0, 4.0), vec2(16.0, 12.0)), 2.0, Color32::from_rgb(20, 20, 24));
    p.rect_filled(egui::Rect::from_min_size(rect.min + vec2(7.0, 22.0), vec2(12.0, 5.0)), 1.5, Color32::from_rgb(20, 20, 24));
}

fn stepper(ui: &mut Ui, cur: usize) {
    ui.horizontal(|ui| {
        for (i, name) in ["Connect", "Choose files", "Install"].iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), egui::Sense::hover());
            let col = if i < cur { GREEN } else if i == cur { ACCENT } else { CARD2 };
            ui.painter().circle_filled(rect.center(), 14.0, col);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                (i + 1).to_string(),
                egui::FontId::proportional(14.0),
                if i <= cur { Color32::from_rgb(20, 20, 24) } else { MUTED },
            );
            let c = if i == cur { TEXT } else { MUTED };
            ui.label(RichText::new(*name).color(c).strong());
            if i < 2 {
                let (r, _) = ui.allocate_exact_size(vec2(46.0, 28.0), egui::Sense::hover());
                let lc = if i < cur { GREEN } else { BORDER };
                ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(2.0, lc));
            }
        }
    });
}

// ---------- The app ----------

struct App {
    step: Step,
    creds: Creds,
    entries: Vec<Entry>,
    folder: Option<PathBuf>,
    local: Vec<LocalFile>,
    plan: Vec<PlanItem>,
    ok: Vec<String>,
    failed: Vec<String>,
    installed_once: bool,
    busy: Option<String>,
    progress: Option<(usize, usize, String)>,
    error: Option<String>,
    note: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            step: Step::Connect,
            creds: Creds { host: String::new(), user: "root".into(), password: "linux".into() },
            entries: vec![],
            folder: None,
            local: vec![],
            plan: vec![],
            ok: vec![],
            failed: vec![],
            installed_once: false,
            busy: None,
            progress: None,
            error: None,
            note: None,
            tx,
            rx,
        }
    }

    /// Fake data so the README screenshots can be made without a Batocera device.
    /// Run with CARTRIDGE_DEMO=connect|pick|install|done
    fn demo(kind: &str) -> Self {
        let mut a = Self::new();
        a.creds.host = "192.168.1.50".into();
        let e = |s: &str, m: bool, f: &str| Entry { system: s.into(), missing: m, md5s: vec!["00".into()], file: f.into() };
        a.entries = vec![
            e("Sony PlayStation", true, "bios/example_psx_1.bin"),
            e("Sony PlayStation", true, "bios/example_psx_2.bin"),
            e("Sega Saturn", true, "bios/example_saturn.bin"),
            e("Sega CD", false, "bios/example_segacd.bin"),
            e("Nintendo DS", true, "bios/example_nds_arm9.bin"),
            e("Neo Geo", true, "bios/example_neogeo.zip"),
        ];
        let ents = a.entries.clone();
        let item = move |i: usize| PlanItem {
            local: PathBuf::from(ents[i].file.replace("bios/", "")),
            remote_rel: ents[i].file.clone(),
            system: ents[i].system.clone(),
        };
        match kind {
            "pick" | "install" | "done" => {
                a.step = Step::Pick;
                a.folder = Some(PathBuf::from("C:/Users/you/Documents/MyBiosDumps"));
                a.local = vec![
                    LocalFile { path: "a".into(), name_lower: "a".into(), md5: "0".into() };
                    24
                ];
                a.plan = vec![item(0), item(1), item(2), item(4)];
                if kind != "pick" {
                    a.step = Step::Install;
                }
                if kind == "done" {
                    a.installed_once = true;
                    a.ok = a.plan.iter().map(|p| p.remote_rel.clone()).collect();
                    a.entries = vec![a.entries[3].clone(), a.entries[5].clone()];
                }
            }
            _ => {}
        }
        a
    }

    fn spawn<F: FnOnce(Sender<Msg>) + Send + 'static>(&mut self, label: &str, ctx: &egui::Context, f: F) {
        self.busy = Some(label.to_string());
        self.error = None;
        self.progress = None;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            f(tx);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            if let Msg::Progress(d, t, n) = m {
                self.progress = Some((d, t, n));
                continue;
            }
            self.busy = None;
            self.progress = None;
            match m {
                Msg::Connected(entries) => {
                    self.entries = entries;
                    self.step = Step::Pick;
                }
                Msg::Local(files) => {
                    self.local = files;
                    self.plan = make_plan(&self.entries, &self.local);
                }
                Msg::Installed { ok, failed, remaining } => {
                    self.ok = ok;
                    self.failed = failed;
                    self.entries = remaining;
                    self.installed_once = true;
                }
                Msg::Err(e) => self.error = Some(e),
                Msg::Progress(..) => {}
            }
        }
    }

    fn start_scan(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.folder = Some(dir.clone());
        self.local.clear();
        self.plan.clear();
        self.spawn("Reading and checking your files...", ctx, move |tx| {
            let _ = tx.send(Msg::Local(scan_folder(&dir)));
        });
    }

    fn report_text(&self) -> String {
        let mut s = String::from("Cartridge report: BIOS files Batocera still needs\n\n");
        let mut by: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
        for e in &self.entries {
            by.entry(e.system.as_str()).or_default().push(e);
        }
        for (sys, list) in by {
            s += &format!("{sys}\n");
            for e in list {
                let md5 = if e.md5s.is_empty() { "-".to_string() } else { e.md5s.join(" or ") };
                let st = if e.missing { "MISSING" } else { "WRONG VERSION" };
                s += &format!("  [{st}] {}   md5: {md5}\n", e.file);
            }
            s += "\n";
        }
        s
    }

    fn save_report(&mut self) {
        if let Some(path) = rfd::FileDialog::new().set_file_name("cartridge-report.txt").save_file() {
            self.note = Some(match std::fs::write(&path, self.report_text()) {
                Ok(()) => format!("Report saved to {}", path.display()),
                Err(e) => format!("Could not save report: {e}"),
            });
        }
    }

    // ----- Step 1 -----
    fn step_connect(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.label(RichText::new("Connect to your Batocera").size(20.0).strong());
        ui.label(RichText::new("Make sure Batocera is on and on the same network as this computer.").color(MUTED));
        ui.add_space(4.0);
        let mut go = false;
        card(ui, |ui| {
            egui::Grid::new("creds").num_columns(2).spacing([14.0, 12.0]).min_col_width(90.0).show(ui, |ui| {
                ui.label("IP address");
                ui.add(egui::TextEdit::singleline(&mut self.creds.host).hint_text("192.168.1.50 or batocera.local").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Username");
                ui.add(egui::TextEdit::singleline(&mut self.creds.user).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.creds.password).password(true).desired_width(f32::INFINITY));
                ui.end_row();
            });
            ui.add_space(2.0);
            ui.label(RichText::new("Tip: your IP is in Batocera under Main menu > Network Settings. The defaults are root / linux.").color(MUTED).small());
            ui.add_space(6.0);
            let can = self.busy.is_none() && !self.creds.host.trim().is_empty();
            go = primary(ui, can, "Connect and check for missing BIOS files");
        });
        if go {
            let c = self.creds.clone();
            self.spawn("Connecting and asking Batocera what is missing...", ctx, move |tx| {
                let r = connect(&c).and_then(|s| fetch_missing(&s));
                let _ = tx.send(match r {
                    Ok(e) => Msg::Connected(e),
                    Err(e) => Msg::Err(e),
                });
            });
        }
    }

    // ----- Step 2 -----
    fn step_pick(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.label(RichText::new("Choose your BIOS folder").size(20.0).strong());
        if self.entries.is_empty() {
            ui.colored_label(GREEN, "Batocera reports no missing BIOS files. You're all set!");
        } else {
            ui.label(RichText::new("Pick a folder with BIOS files you dumped from consoles you own (subfolders are searched too). You can also drag a folder onto this window.").color(MUTED));
        }
        let found = self.plan.len();
        ui.horizontal(|ui| {
            stat(ui, self.entries.len(), "needed by Batocera", YELLOW);
            stat(ui, found, "found in your folder", GREEN);
            stat(ui, self.entries.len().saturating_sub(found), "still missing", RED);
        });

        let mut choose = false;
        card(ui, |ui| {
            ui.horizontal(|ui| {
                choose = ui.add_enabled(self.busy.is_none(), egui::Button::new("Choose folder...").min_size(vec2(0.0, 36.0))).clicked();
                match &self.folder {
                    Some(f) => {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(f.display().to_string()).strong());
                            if self.busy.is_none() {
                                ui.label(RichText::new(format!("{} files scanned", self.local.len())).color(MUTED).small());
                            }
                        });
                    }
                    None => {
                        ui.label(RichText::new("No folder chosen yet").color(MUTED));
                    }
                }
            });
        });
        if choose {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.start_scan(dir, ctx);
            }
        }

        card(ui, |ui| {
            egui::ScrollArea::vertical().max_height(190.0).auto_shrink([false, true]).show(ui, |ui| {
                let mut by: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
                for e in &self.entries {
                    by.entry(e.system.as_str()).or_default().push(e);
                }
                for (sys, list) in by {
                    ui.label(RichText::new(sys).strong().color(ACCENT));
                    for e in list {
                        ui.horizontal(|ui| {
                            if self.plan.iter().any(|p| p.remote_rel == e.file) {
                                badge(ui, "FOUND", GREEN);
                            } else if e.missing {
                                badge(ui, "MISSING", RED);
                            } else {
                                badge(ui, "WRONG VERSION", YELLOW);
                            }
                            ui.label(RichText::new(&e.file).color(MUTED));
                        });
                    }
                    ui.add_space(4.0);
                }
            });
        });

        let (mut back, mut next) = (false, false);
        ui.horizontal(|ui| {
            back = ui.button("Back").clicked();
            next = primary(ui, !self.plan.is_empty() && self.busy.is_none(), &format!("Next: install {} file(s)", self.plan.len()));
            if ui.button("Save list of what's needed").clicked() {
                self.save_report();
            }
        });
        if back {
            self.step = Step::Connect;
        }
        if next {
            self.step = Step::Install;
        }
    }

    // ----- Step 3 -----
    fn step_install(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        if !self.installed_once {
            ui.label(RichText::new("Ready to install").size(20.0).strong());
            ui.label(RichText::new("These files will be copied to /userdata/bios/ on your Batocera.").color(MUTED));
            card(ui, |ui| {
                egui::ScrollArea::vertical().max_height(250.0).auto_shrink([false, true]).show(ui, |ui| {
                    for p in &self.plan {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(p.local.file_name().unwrap_or_default().to_string_lossy()).strong());
                            ui.label(RichText::new("->").color(MUTED));
                            ui.label(RichText::new(&p.remote_rel).color(ACCENT));
                        });
                    }
                });
            });
            if let Some((d, t, name)) = &self.progress {
                card(ui, |ui| {
                    ui.add(egui::ProgressBar::new(*d as f32 / (*t).max(1) as f32).fill(ACCENT).text(format!("{d} / {t}")));
                    ui.label(RichText::new(format!("Uploading {name}")).color(MUTED).small());
                });
            }
            let (mut back, mut go) = (false, false);
            ui.horizontal(|ui| {
                back = ui.add_enabled(self.busy.is_none(), egui::Button::new("Back")).clicked();
                go = primary(ui, self.busy.is_none(), "Install now");
            });
            if back {
                self.step = Step::Pick;
            }
            if go {
                let (c, plan) = (self.creds.clone(), self.plan.clone());
                self.spawn("Uploading files...", ctx, move |tx| {
                    let r = connect(&c).and_then(|s| {
                        let tx2 = tx.clone();
                        let (ok, failed) = upload(&s, &plan, &mut |d, t, n| {
                            let _ = tx2.send(Msg::Progress(d, t, n.to_string()));
                        });
                        let remaining = fetch_missing(&s)?;
                        Ok(Msg::Installed { ok, failed, remaining })
                    });
                    let _ = tx.send(r.unwrap_or_else(Msg::Err));
                });
            }
        } else {
            let all = self.entries.is_empty();
            ui.label(RichText::new(if all { "All done!" } else { "Installed" }).size(20.0).strong());
            ui.horizontal(|ui| {
                stat(ui, self.ok.len(), "files installed", GREEN);
                stat(ui, self.failed.len(), "failed", if self.failed.is_empty() { MUTED } else { RED });
                stat(ui, self.entries.len(), "still missing", if all { MUTED } else { YELLOW });
            });
            card(ui, |ui| {
                if all {
                    ui.colored_label(GREEN, "Batocera now reports no missing BIOS files.");
                    ui.label("Restart EmulationStation (or reboot Batocera) and your games are ready to go.");
                } else {
                    ui.label("Batocera still needs these. You'll have to dump them yourself:");
                    egui::ScrollArea::vertical().max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
                        for e in &self.entries {
                            ui.horizontal(|ui| {
                                badge(ui, if e.missing { "MISSING" } else { "WRONG VERSION" }, if e.missing { RED } else { YELLOW });
                                ui.label(RichText::new(format!("{}: {}", e.system, e.file)).color(MUTED));
                            });
                        }
                    });
                }
                for f in &self.failed {
                    ui.colored_label(RED, format!("Failed: {f}"));
                }
            });
            let mut again = false;
            ui.horizontal(|ui| {
                again = primary(ui, true, "Start over");
                if !all && ui.button("Save list of what's needed").clicked() {
                    self.save_report();
                }
            });
            if again {
                self.step = Step::Connect;
                self.installed_once = false;
                self.plan.clear();
                self.local.clear();
                self.folder = None;
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        // Drag a folder onto the window
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            if p.is_dir() && self.step == Step::Pick && self.busy.is_none() {
                self.start_scan(p, ctx);
            }
        }
        if self.busy.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(22.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    logo(ui);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("Cartridge").size(24.0).strong());
                        ui.label(RichText::new("BIOS installer for Batocera").color(MUTED).small());
                    });
                });
                ui.add_space(6.0);
                let cur = match self.step {
                    Step::Connect => 0,
                    Step::Pick => 1,
                    Step::Install => 2,
                };
                stepper(ui, cur);
                ui.add_space(6.0);
                match self.step {
                    Step::Connect => self.step_connect(ui, ctx),
                    Step::Pick => self.step_pick(ui, ctx),
                    Step::Install => self.step_install(ui, ctx),
                }
                if let Some(b) = &self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(b).color(MUTED));
                    });
                }
                if let Some(e) = &self.error {
                    card(ui, |ui| {
                        ui.colored_label(RED, e);
                    });
                }
                if let Some(n) = &self.note {
                    ui.label(RichText::new(n).color(GREEN).small());
                }
            });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 660.0])
            .with_min_inner_size([640.0, 560.0])
            .with_app_id("cartridge") // lets Linux match the window to cartridge.desktop
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon_256.rgba").to_vec(),
                width: 256,
                height: 256,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "Cartridge",
        options,
        Box::new(|cc| {
            apply_style(&cc.egui_ctx);
            Box::new(match std::env::var("CARTRIDGE_DEMO") {
                Ok(k) => App::demo(&k),
                Err(_) => App::new(),
            })
        }),
    )
}
