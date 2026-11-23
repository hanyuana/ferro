#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use clap::Parser;
use eframe::egui;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FERRO_EXE_BYTES: &[u8] = include_bytes!("../../ferro.exe");
const FERRO_ICO_BYTES: &[u8] = include_bytes!("../../assets/ferro.ico");
const ICON_64_RGBA: &[u8] = include_bytes!("../../assets/icon_64_rgba.bin");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug, Clone)]
#[command(name = "ferro-setup", version, about = "Ferro Windows x64 安装与卸载程序")]
struct SetupCli {
    /// 自定义安装目录（默认：%LOCALAPPDATA%\Programs\Ferro）
    #[arg(long)]
    install_dir: Option<PathBuf>,

    /// 自定义开始菜单快捷方式目录（默认：%APPDATA%\Microsoft\Windows\Start Menu\Programs）
    #[arg(long)]
    start_menu_dir: Option<PathBuf>,

    /// 不创建开始菜单快捷方式
    #[arg(long)]
    no_shortcuts: bool,

    /// 执行卸载（仅移除程序与快捷方式，保留用户会话与配置）
    #[arg(long)]
    uninstall: bool,

    /// 静默执行安装或卸载并输出 JSON 结果（用于自动化验证与无界面部署）
    #[arg(long)]
    silent: bool,
}

fn main() -> Result<()> {
    #[cfg(windows)]
    if std::env::args_os().len() > 1 {
        extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
            fn GetStdHandle(nStdHandle: u32) -> isize;
        }
        unsafe {
            const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h == 0 || h == -1 {
                AttachConsole(u32::MAX);
            }
        }
    }

    let mut cli = SetupCli::parse();
    let exe_path = std::env::current_exe().ok();
    let invoked_as_uninstall = exe_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_lowercase().contains("uninstall"))
        .unwrap_or(false);
    if invoked_as_uninstall {
        cli.uninstall = true;
        if cli.install_dir.is_none() {
            cli.install_dir = exe_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        }
    }

    let install_dir = cli.install_dir.clone().unwrap_or_else(default_install_dir);
    let custom_start_menu = cli.start_menu_dir.is_some();
    let start_menu_dir = cli.start_menu_dir.clone().unwrap_or_else(default_start_menu_dir);

    if cli.silent {
        if cli.uninstall {
            let summary = perform_uninstall(&install_dir, &start_menu_dir, !custom_start_menu)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            let summary = perform_install(
                &install_dir,
                &start_menu_dir,
                !cli.no_shortcuts,
                !custom_start_menu,
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        return Ok(());
    }

    run_installer_gui(install_dir, start_menu_dir, !cli.no_shortcuts, cli.uninstall, !custom_start_menu)
}

fn default_install_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Programs")
        .join("Ferro")
}

fn default_start_menu_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
}

fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: ICON_64_RGBA.to_vec(),
        width: 64,
        height: 64,
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for p in ["C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/msyh.ttf", "C:/Windows/Fonts/simhei.ttf"] {
        if let Ok(bytes) = std::fs::read(p) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".into());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

struct InstallerApp {
    install_dir: String,
    start_menu_dir: PathBuf,
    create_shortcut: bool,
    launch_after: bool,
    uninstall_mode: bool,
    register_hkcu: bool,
    status: String,
    done: bool,
    logo_tex: Option<egui::TextureHandle>,
    folder_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    error_details: Option<String>,
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.folder_rx {
            match rx.try_recv() {
                Ok(chosen) => {
                    if let Some(path) = chosen {
                        self.install_dir = path.display().to_string();
                    }
                    self.folder_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.folder_rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if self.logo_tex.is_none() {
            let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], ICON_64_RGBA);
            self.logo_tex = Some(ctx.load_texture("ferro_logo", img, egui::TextureOptions::LINEAR));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if let Some(tex) = &self.logo_tex {
                    ui.image((tex.id(), egui::vec2(48.0, 48.0)));
                }
                ui.vertical(|ui| {
                    ui.heading(
                        egui::RichText::new(format!("Ferro v{} 安装程序 (Windows x64)", APP_VERSION))
                            .color(egui::Color32::from_rgb(94, 234, 212)),
                    );
                    ui.label("轻量纯原生 Rust AI 编码桌面客户端 · 无需账号登录 · 不依赖浏览器或 Magpie");
                });
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            ui.label("安装目录（默认按当前用户安装，无需管理员权限）：");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.install_dir).desired_width(410.0));
                if ui.add_enabled(self.folder_rx.is_none(), egui::Button::new("浏览…")).clicked() {
                    let initial = PathBuf::from(&self.install_dir);
                    let mut dialog = rfd::FileDialog::new().set_title("选择安装目录");
                    if initial.is_dir() {
                        dialog = dialog.set_directory(&initial);
                    }
                    #[cfg(windows)]
                    {
                        let (tx, rx) = std::sync::mpsc::channel();
                        self.folder_rx = Some(rx);
                        let repaint = ctx.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(dialog.pick_folder());
                            repaint.request_repaint();
                        });
                    }
                    #[cfg(not(windows))]
                    if let Some(chosen) = dialog.set_parent(_frame).pick_folder() {
                        self.install_dir = chosen.display().to_string();
                    }
                }
            });

            ui.add_space(10.0);
            ui.checkbox(&mut self.create_shortcut, "创建开始菜单快捷方式 (Ferro)");
            ui.checkbox(&mut self.launch_after, "安装完成后立即启动 Ferro");

            ui.add_space(8.0);
            ui.small("提示：升级安装或卸载均会完整保留您的历史会话与本地模型配置 (%APPDATA%\\ferro)。");

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let install_label = if self.uninstall_mode { "重新安装 / 修复" } else { "立即安装 / 升级" };
                if ui.add_enabled(self.folder_rx.is_none(), egui::Button::new(egui::RichText::new(install_label).strong())).clicked() {
                    self.error_details = None;
                    let target = PathBuf::from(self.install_dir.trim());
                    match perform_install(&target, &self.start_menu_dir, self.create_shortcut, self.register_hkcu) {
                        Ok(_) => {
                            self.status = format!("✓ 已成功安装到：{}", target.display());
                            self.done = true;
                            if self.launch_after {
                                let exe = target.join("ferro.exe");
                                let _ = std::process::Command::new(exe).current_dir(&target).spawn();
                            }
                        }
                        Err(e) => {
                            self.done = false;
                            self.status = "安装失败。请检查目录是否可写，以及 Ferro 是否仍在运行。".into();
                            self.error_details = Some(format!("{e:#}"));
                        }
                    }
                }

                if ui.add_enabled(self.folder_rx.is_none(), egui::Button::new("卸载已安装版本")).clicked() {
                    self.error_details = None;
                    let target = PathBuf::from(self.install_dir.trim());
                    match perform_uninstall(&target, &self.start_menu_dir, self.register_hkcu) {
                        Ok(_) => {
                            self.status = format!("✓ 已从 {} 卸载程序与快捷方式（用户会话与配置已保留）", target.display());
                            self.done = true;
                        }
                        Err(e) => {
                            self.done = false;
                            self.status = "卸载失败。请检查目录权限和程序运行状态。".into();
                            self.error_details = Some(format!("{e:#}"));
                        }
                    }
                }

                if ui.button("关闭").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            if !self.status.is_empty() {
                ui.add_space(14.0);
                let color = if self.done {
                    egui::Color32::from_rgb(74, 222, 128)
                } else {
                    egui::Color32::from_rgb(248, 113, 113)
                };
                ui.colored_label(color, &self.status);
                if let Some(details) = &self.error_details {
                    ui.collapsing("原始详情", |ui| {
                        egui::ScrollArea::vertical().max_height(70.0).show(ui, |ui| {
                            ui.add(egui::Label::new(details).wrap().selectable(true));
                        });
                    });
                }
            }
        });
    }
}

fn run_installer_gui(
    install_dir: PathBuf,
    start_menu_dir: PathBuf,
    create_shortcut: bool,
    uninstall_mode: bool,
    register_hkcu: bool,
) -> Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("Ferro v{} 安装向导", APP_VERSION))
            .with_icon(Arc::new(app_icon()))
            .with_inner_size([585.0, 400.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "Ferro 安装向导",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(InstallerApp {
                install_dir: install_dir.display().to_string(),
                start_menu_dir,
                create_shortcut,
                launch_after: false,
                uninstall_mode,
                register_hkcu,
                status: String::new(),
                done: false,
                logo_tex: None,
                folder_rx: None,
                error_details: None,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("安装窗口创建失败: {e}"))
}

fn perform_install(
    install_dir: &Path,
    start_menu_dir: &Path,
    create_shortcuts: bool,
    register_hkcu: bool,
) -> Result<serde_json::Value> {
    std::fs::create_dir_all(install_dir)
        .with_context(|| format!("无法创建安装目录: {}", install_dir.display()))?;

    let target_exe = install_dir.join("ferro.exe");
    let target_ico = install_dir.join("ferro.ico");
    let target_uninst = install_dir.join("uninstall.exe");

    std::fs::write(&target_exe, FERRO_EXE_BYTES)
        .with_context(|| format!("无法写入 {}", target_exe.display()))?;
    std::fs::write(&target_ico, FERRO_ICO_BYTES)
        .with_context(|| format!("无法写入 {}", target_ico.display()))?;

    if let Ok(self_exe) = std::env::current_exe() {
        if self_exe != target_uninst {
            let _ = std::fs::copy(&self_exe, &target_uninst);
        }
    }

    let mut shortcut_path: Option<PathBuf> = None;
    let mut uninst_shortcut_path: Option<PathBuf> = None;
    if create_shortcuts {
        std::fs::create_dir_all(start_menu_dir)?;
        let lnk = start_menu_dir.join("Ferro.lnk");
        create_windows_shortcut(
            &lnk,
            &target_exe,
            "",
            install_dir,
            &target_ico,
            "Ferro — 极简原生 Rust AI 编码桌面客户端",
        )?;
        shortcut_path = Some(lnk);

        if target_uninst.exists() {
            let ulnk = start_menu_dir.join("卸载 Ferro.lnk");
            let args = format!("--uninstall --install-dir \"{}\"", install_dir.display());
            let _ = create_windows_shortcut(
                &ulnk,
                &target_uninst,
                &args,
                install_dir,
                &target_ico,
                "卸载 Ferro",
            );
            uninst_shortcut_path = Some(ulnk);
        }
    }

    if register_hkcu {
        let _ = register_uninstall_entry(install_dir, &target_ico, &target_uninst);
    }

    Ok(json!({
        "action": "install",
        "version": APP_VERSION,
        "install_dir": install_dir.display().to_string(),
        "ferro_exe": target_exe.display().to_string(),
        "ferro_ico": target_ico.display().to_string(),
        "uninstall_exe": target_uninst.display().to_string(),
        "shortcut": shortcut_path.map(|p| p.display().to_string()),
        "uninstall_shortcut": uninst_shortcut_path.map(|p| p.display().to_string()),
    }))
}

fn perform_uninstall(
    install_dir: &Path,
    start_menu_dir: &Path,
    unregister_hkcu: bool,
) -> Result<serde_json::Value> {
    let target_exe = install_dir.join("ferro.exe");
    let target_ico = install_dir.join("ferro.ico");
    let target_uninst = install_dir.join("uninstall.exe");
    let lnk = start_menu_dir.join("Ferro.lnk");
    let ulnk = start_menu_dir.join("卸载 Ferro.lnk");
    let old_ulnk = start_menu_dir.join("Uninstall Ferro.lnk");

    let mut removed = Vec::new();
    for p in [&target_exe, &target_ico, &lnk, &ulnk, &old_ulnk] {
        if p.exists() && std::fs::remove_file(p).is_ok() {
            removed.push(p.display().to_string());
        }
    }

    let self_exe = std::env::current_exe().ok();
    if target_uninst.exists() {
        if self_exe.as_deref() != Some(target_uninst.as_path()) {
            if std::fs::remove_file(&target_uninst).is_ok() {
                removed.push(target_uninst.display().to_string());
            }
        } else {
            // Schedule delayed removal of uninstall.exe after process exit
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let cmd_str = format!(
                    "ping 127.0.0.1 -n 2 > nul & del /f /q \"{}\" & rmdir \"{}\" 2>nul",
                    target_uninst.display(),
                    install_dir.display()
                );
                let _ = std::process::Command::new("cmd.exe")
                    .creation_flags(0x0800_0000)
                    .args(["/C", &cmd_str])
                    .spawn();
                removed.push(target_uninst.display().to_string());
            }
        }
    }

    // Remove install_dir if it is now empty (never forces recursive delete on user files)
    let _ = std::fs::remove_dir(install_dir);

    if unregister_hkcu {
        let _ = unregister_uninstall_entry();
    }

    Ok(json!({
        "action": "uninstall",
        "install_dir": install_dir.display().to_string(),
        "removed": removed,
        "user_data_preserved": true,
    }))
}

fn create_windows_shortcut(
    lnk_path: &Path,
    target_path: &Path,
    arguments: &str,
    working_dir: &Path,
    icon_path: &Path,
    description: &str,
) -> Result<()> {
    let esc = |s: &str| s.replace('\'', "''");
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.Arguments = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.IconLocation = '{},0'; \
         $s.Description = '{}'; \
         $s.Save()",
        esc(&lnk_path.display().to_string()),
        esc(&target_path.display().to_string()),
        esc(arguments),
        esc(&working_dir.display().to_string()),
        esc(&icon_path.display().to_string()),
        esc(description),
    );
    let mut cmd = std::process::Command::new("powershell.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let status = cmd
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("无法调用 PowerShell 创建快捷方式")?;
    anyhow::ensure!(status.success() && lnk_path.exists(), "创建快捷方式失败: {}", lnk_path.display());
    Ok(())
}

fn register_uninstall_entry(install_dir: &Path, icon_path: &Path, uninst_path: &Path) -> Result<()> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ferro";
    let uninst_cmd = format!("\"{}\" --uninstall --install-dir \"{}\"", uninst_path.display(), install_dir.display());
    for (name, kind, val) in [
        ("DisplayName", "REG_SZ", "Ferro"),
        ("DisplayVersion", "REG_SZ", APP_VERSION),
        ("Publisher", "REG_SZ", "Ferro 贡献者"),
        ("InstallLocation", "REG_SZ", &install_dir.display().to_string()),
        ("DisplayIcon", "REG_SZ", &icon_path.display().to_string()),
        ("UninstallString", "REG_SZ", &uninst_cmd),
        ("NoModify", "REG_DWORD", "1"),
        ("NoRepair", "REG_DWORD", "1"),
    ] {
        let mut cmd = std::process::Command::new("reg.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let _ = cmd.args(["add", key, "/v", name, "/t", kind, "/d", val, "/f"]).status();
    }
    Ok(())
}

fn unregister_uninstall_entry() -> Result<()> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ferro";
    let mut cmd = std::process::Command::new("reg.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.args(["delete", key, "/f"]).status();
    Ok(())
}
