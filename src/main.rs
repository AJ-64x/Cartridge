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

use eframe::egui;
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

fn upload(sess: &Session, plan: &[PlanItem]) -> (Vec<String>, Vec<String>) {
    let (mut ok, mut failed) = (vec![], vec![]);
    let sftp = match sess.sftp() {
        Ok(s) => s,
        Err(e) => return (ok, plan.iter().map(|p| format!("{} ({e})", p.remote_rel)).collect()),
    };
    for item in plan {
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
    error: Option<String>,
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
            error: None,
            tx,
            rx,
        }
    }

    fn spawn<F: FnOnce(Sender<Msg>) + Send + 'static>(&mut self, label: &str, ctx: &egui::Context, f: F) {
        self.busy = Some(label.to_string());
        self.error = None;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            f(tx);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.busy = None;
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
            }
        }
    }

    fn step_connect(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Step 1: Connect to your Batocera");
        ui.label("Find your Batocera's IP address in Batocera: Main menu > Network Settings.");
        ui.add_space(8.0);
        egui::Grid::new("creds").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("IP address");
            ui.add(egui::TextEdit::singleline(&mut self.creds.host).hint_text("e.g. 192.168.1.50 or batocera.local"));
            ui.end_row();
            ui.label("Username");
            ui.text_edit_singleline(&mut self.creds.user);
            ui.end_row();
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut self.creds.password).password(true));
            ui.end_row();
        });
        ui.label("Defaults for Batocera are user \"root\" and password \"linux\".");
        ui.add_space(12.0);
        let can = self.busy.is_none() && !self.creds.host.trim().is_empty();
        if ui.add_enabled(can, egui::Button::new("Connect and check for missing BIOS files")).clicked() {
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

    fn step_pick(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Step 2: Pick your BIOS folder");
        let total = self.entries.len();
        if total == 0 {
            ui.colored_label(egui::Color32::LIGHT_GREEN, "Batocera reports no missing BIOS files. You're all set!");
        } else {
            ui.label(format!("Batocera is missing {total} BIOS file(s) for the systems/emulators you have."));
            ui.label("Choose a folder on this computer that holds BIOS files you dumped from consoles you own. Subfolders are searched too.");
        }
        ui.add_space(8.0);
        if ui.add_enabled(self.busy.is_none(), egui::Button::new("Choose folder...")).clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.folder = Some(dir.clone());
                self.local.clear();
                self.plan.clear();
                self.spawn("Reading and checking your files...", ctx, move |tx| {
                    let _ = tx.send(Msg::Local(scan_folder(&dir)));
                });
            }
        }
        if let Some(f) = &self.folder {
            ui.label(format!("Folder: {}", f.display()));
            if self.busy.is_none() {
                ui.label(format!("Scanned {} file(s). {} match what Batocera needs.", self.local.len(), self.plan.len()));
            }
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(230.0).show(ui, |ui| {
            for e in &self.entries {
                let found = self.plan.iter().any(|p| p.remote_rel == e.file);
                let (mark, color) = if found {
                    ("found", egui::Color32::LIGHT_GREEN)
                } else if e.missing {
                    ("missing", egui::Color32::LIGHT_RED)
                } else {
                    ("wrong version", egui::Color32::YELLOW)
                };
                ui.horizontal(|ui| {
                    ui.colored_label(color, format!("[{mark}]"));
                    ui.label(format!("{}: {}", e.system, e.file));
                });
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                self.step = Step::Connect;
            }
            if ui.add_enabled(!self.plan.is_empty() && self.busy.is_none(), egui::Button::new(format!("Next: install {} file(s)", self.plan.len()))).clicked() {
                self.step = Step::Install;
            }
        });
    }

    fn step_install(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Step 3: Install");
        if !self.installed_once {
            ui.label("These files will be copied to /userdata/bios/ on your Batocera:");
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for p in &self.plan {
                    ui.label(format!("{}  ->  {}", p.local.file_name().unwrap_or_default().to_string_lossy(), p.remote_rel));
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("Back")).clicked() {
                    self.step = Step::Pick;
                }
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("Install now")).clicked() {
                    let (c, plan) = (self.creds.clone(), self.plan.clone());
                    self.spawn("Uploading files...", ctx, move |tx| {
                        let r = connect(&c).and_then(|s| {
                            let (ok, failed) = upload(&s, &plan);
                            let remaining = fetch_missing(&s)?;
                            Ok(Msg::Installed { ok, failed, remaining })
                        });
                        let _ = tx.send(r.unwrap_or_else(Msg::Err));
                    });
                }
            });
        } else {
            ui.colored_label(egui::Color32::LIGHT_GREEN, format!("Installed {} file(s).", self.ok.len()));
            for f in &self.failed {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Failed: {f}"));
            }
            ui.add_space(6.0);
            if self.entries.is_empty() {
                ui.label("Batocera now reports no missing BIOS files. Restart EmulationStation or reboot Batocera and enjoy!");
            } else {
                ui.label(format!("Still missing {} file(s) (shown below). You'll need to dump those yourself:", self.entries.len()));
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for e in &self.entries {
                        ui.label(format!("{}: {}", e.system, e.file));
                    }
                });
            }
            ui.add_space(8.0);
            if ui.button("Start over").clicked() {
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Cartridge");
                ui.label("BIOS installer for Batocera");
            });
            ui.separator();
            match self.step {
                Step::Connect => self.step_connect(ui, ctx),
                Step::Pick => self.step_pick(ui, ctx),
                Step::Install => self.step_install(ui, ctx),
            }
            ui.add_space(10.0);
            if let Some(b) = &self.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(b);
                });
            }
            if let Some(e) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([700.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Cartridge", options, Box::new(|_cc| Box::new(App::new())))
}
