#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/* 阴影留白：窗口比卡片四周各宽 20px，卡片由前端撑满 */
const PAD: f64 = 20.0;

/* 尺寸硬边界：真正的 clamp 在前端按下限，这里再兜一道，防止任何路径把窗口拉爆 */
const MIN_W: f64 = 280.0;
const MAX_W: f64 = 600.0;
const MIN_H: f64 = 100.0;
const MAX_H: f64 = 1000.0;

/* 启动即贴合态：贴在桌面上、鼠标穿透，按住 Ctrl 才活 */
static PINNED: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
/* 设置面板打开时：强制置顶、强制不穿透，也不让 Ctrl 轮询去改它 */
static SETTINGS: AtomicBool = AtomicBool::new(false);

/* 正在输入文字。
   贴合态一旦开启鼠标穿透，窗口就失去激活，WebView 立刻 blur ——
   正在编辑的那一条会被当场提交、输入框消失。所以打字期间必须继续可点。 */
static TYPING: AtomicBool = AtomicBool::new(false);

/* 为了「临时」打字而顶上来的置顶态（Ctrl+Alt+N 从别的程序里按下来）。
   打完这一条要自动退回贴合态并沉底，不能把窗口永久留在最上层。 */
static TEMP_PIN: AtomicBool = AtomicBool::new(false);

/* 指针是否悬在卡片上 —— 贴合态用它做「悬停唤醒」：
   底衬和操作层浮上来，不必先按 Ctrl。 */
static HOVER: AtomicBool = AtomicBool::new(false);

/* 清单是否溢出可滚。只有溢出时，悬停才真的接管鼠标（滚轮要能滚），
   否则保持穿透，不挡桌面上的东西。 */
static SCROLLABLE: AtomicBool = AtomicBool::new(false);

/* 窗口此刻是不是「鼠标穿透」状态。
   穿透时系统不会把滚轮投递给 WebView（窗口命中测试直接跳过我们），
   唯一能滚的办法就是低层钩子 —— 所以这个状态必须让滚轮那一路知道，
   否则会出现「有焦点 + 穿透 = 谁也滚不动」的死角。 */
static IGNORING: AtomicBool = AtomicBool::new(true);

/* 「刚贴回桌面」的抑制闩。
   Ctrl+Alt+D 本身就要按着 Ctrl，热键回到贴合态的那一瞬间 Ctrl 还压在手里，
   轮询线程下一次取样会把状态又推成「激活态」，画面就卡在半透明白卡上。
   所以回到贴合态后先上闩，等 Ctrl 真正松开再摘闩，中间一律按「没按」处理。 */
static SUPPRESS: AtomicBool = AtomicBool::new(false);

/* 「刚被双击图标叫上来」的短暂置顶期。
   这 1.4 秒里绝不能再压底 —— SetWindowPos(HWND_BOTTOM) 会顺手把 TOPMOST
   样式摘掉，浮动还没被看见就被自己踩回去了。 */
static NUDGE: AtomicBool = AtomicBool::new(false);

/* 窗口位置记忆 */
static POS: Mutex<Option<(f64, f64)>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(false);
/* 进设置前的窗口几何（x, y, w, h），退出时还原 */
static PREV: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

/* ── 按住 Ctrl 的探测 ──────────────────────────────────────────
   Windows：GetAsyncKeyState 轮询，不需要管理员权限，也不装钩子
   macOS：CGEventSourceKeyState 读 HID 状态，不需要辅助功能授权
*/
#[cfg(windows)]
fn ctrl_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    unsafe {
        let s = GetAsyncKeyState(VK_CONTROL.0 as i32);
        (s as u16 & 0x8000u16) != 0
    }
}

#[cfg(target_os = "macos")]
fn ctrl_down() -> bool {
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    match CGEventSource::new(CGEventSourceStateID::HIDSystemState) {
        Ok(src) => src.key_state(0x3B) || src.key_state(0x3E), // kVK_Control / kVK_RightControl
        Err(_) => false,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn ctrl_down() -> bool {
    false
}

/* 临时诊断日志：写 %TEMP%\desklist.log */
fn logln(s: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("desklist.log"))
    {
        let _ = writeln!(f, "{}", s);
    }
}

/* ── 窗口位置落盘 ─────────────────────────────────────────── */
fn data_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                std::path::PathBuf::from(h).join("Library/Application Support")
            })
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("DeskList")
}

fn load_pos() -> Option<(f64, f64)> {
    let s = std::fs::read_to_string(data_dir().join("window.pos")).ok()?;
    let mut it = s.split_whitespace();
    let x: f64 = it.next()?.parse().ok()?;
    let y: f64 = it.next()?.parse().ok()?;
    Some((x, y))
}

fn save_pos(x: f64, y: f64) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("window.pos"), format!("{}\n{}\n", x, y));
}

/* ── 清单数据落盘 ───────────────────────────────────────────
   WebView2 的 localStorage 在某些机器/某些时候会静默失效：setItem 不报错，
   但既不写 LevelDB、下次启动也读不回来 —— 表现就是「加的东西第二天没了」。
   所以清单同时写一份到 %APPDATA%\DeskList\data.json：
   —— 启动时 localStorage 读不到内容，就从这个文件恢复；
   —— 每次保存顺手镜像一份，先写 .tmp 再改名，断电也不会留半截文件。 */
fn data_file() -> std::path::PathBuf {
    data_dir().join("data.json")
}

/* 每次启动把上一次的 data.json 留一份 data.backup.json。
   这个文件只是「万一」，界面上永远不读它 —— 但真出过一次写坏数据的事，
   有它在就能把上一版清单捞回来。一个进程只做一次。 */
static BACKUP_DONE: AtomicBool = AtomicBool::new(false);
fn backup_once() {
    if BACKUP_DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let src = data_file();
    if src.exists() {
        let _ = std::fs::copy(&src, data_dir().join("data.backup.json"));
    }
}

#[tauri::command]
fn load_data() -> Option<String> {
    backup_once();
    std::fs::read_to_string(data_file()).ok().filter(|s| s.len() > 2)
}

#[tauri::command]
fn save_data(json: String) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join("data.json.tmp");
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, data_file());
    }
}

/* 上次手动调过的卡片尺寸。启动时先按它把窗口摆好 ——
   否则先按配置里的默认高度起窗，卡片被窗口夹小，前端再把窗口缩回去，
   结果是「每次重开都变小一圈」，用户手动调的高度永远丢。 */
fn saved_card_size() -> Option<(f64, f64)> {
    let s = std::fs::read_to_string(data_file()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let w = v.get("cardW").and_then(|x| x.as_f64()).unwrap_or(380.0);
    let h = v.get("cardH").and_then(|x| x.as_f64())?;
    if !(60.0..=2000.0).contains(&h) || !(60.0..=2000.0).contains(&w) {
        return None;
    }
    Some((
        w.clamp(MIN_W, MAX_W),
        h.clamp(MIN_H, MAX_H),
    ))
}

/* ── 鼠标穿透 ─────────────────────────────────────────────── */
fn apply_mouse(win: &WebviewWindow) {
    let pinned = PINNED.load(Ordering::Relaxed);
    let ctrl = CTRL.load(Ordering::Relaxed);
    let settings = SETTINGS.load(Ordering::Relaxed);
    let typing = TYPING.load(Ordering::Relaxed);
    /* 贴合态默认一律穿透 —— 哪怕指针正停在卡片上。
       悬停只是把底衬和操作层「显示」出来，不交出鼠标：想真的点勾选框，
       必须先按住 Ctrl。滚轮另有低层钩子单独处理，不受穿透影响。 */
    let ignore = !settings && !pinned && !ctrl && !typing;
    let was_ignoring = IGNORING.swap(ignore, Ordering::Relaxed);
    let _ = win.set_ignore_cursor_events(ignore);
    if pinned {
        /* 上面这一下重写了整份 exstyle，WS_EX_TOPMOST 没了 —— 置顶态每次补回来 */
        force_top(win, true);
    } else if ignore && !was_ignoring {
        /* 刚切回穿透态：Windows 重建 layered 样式时会把窗口提到最前。
           不压回去就是「取消置顶后浮在所有窗口之上、回不到桌面」。
           这一步必须排在 set_ignore_cursor_events 之后，否则白沉。 */
        send_to_bottom(win);
    }
}

/* ── 贴到桌面之上 ─────────────────────────────────────────────
   set_always_on_top(false) 只是摘掉 WS_EX_TOPMOST，窗口仍然停在
   「普通窗口的最上层」——所以从置顶态切回贴合态时，它会盖住后面的
   窗口，直到用户点一下桌面、Z 序被重排才落下去。

   但「贴底」不能理解成 Z 序的**绝对**底部：
     · 平常桌面窗口（Progman）本来就在最底，HWND_BOTTOM 恰好落在它上方，看着是对的；
     · 按下 Win+D（显示桌面）时，资源管理器会把 Progman 提到所有窗口**前面**，
       这时候再压到绝对底部，就等于压到桌面**背后** —— 桌面上再也看不到卡片，
       一直到把窗口恢复、桌面重新沉底为止。用户看到的就是「按 Win+D 之后贴合态没了」。
   所以改成「永远插在桌面正上方那一个窗口的后面」：
   桌面之上、所有普通窗口之下，桌面是被抬起来还是沉下去都不影响它。
   位置已经对了就不动，省得每 0.55 秒抖一次 Z 序。 */
#[cfg(windows)]
unsafe extern "system" fn enum_z(
    h: windows::Win32::Foundation::HWND,
    l: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::BOOL {
    let v = &mut *(l.0 as *mut Vec<isize>);
    v.push(h.0 as isize);
    windows::Win32::Foundation::BOOL(1)
}

/* 不可见的系统壳窗不能当 Z 序锚点：
   它们在被枚举之后随时会销毁，SetWindowPos 拿它们当 hwndInsertAfter 会直接失败。
   输入法的 IME / MSCTFIME UI / CicLoaderWndClass 是最常撞上的几个。 */
#[cfg(windows)]
fn is_shell_noise(raw: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, IsWindowVisible};
    unsafe {
        let h = HWND(raw as *mut core::ffi::c_void);
        if IsWindowVisible(h).as_bool() {
            return false; /* 看得见的窗口可以当锚点 */
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(h, &mut buf);
        if n <= 0 {
            return true; /* 连类名都取不到，别用 */
        }
        let cls = String::from_utf16_lossy(&buf[..n as usize]);
        matches!(
            cls.as_str(),
            "CicLoaderWndClass"
                | "MSCTFIME UI"
                | "IME"
                | "Default IME"
                | "Windows.UI.Core.CoreWindow"
                | "ForegroundStaging"
                | "tooltips_class32"
                | "GlobalUserData"
                | "Base_PowerMessageWindow"
        )
    }
}

#[cfg(not(windows))]
fn is_shell_noise(_raw: isize) -> bool {
    false
}

#[cfg(windows)]
fn bottom_at(raw: isize) -> bool {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetShellWindow, SetWindowPos, HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE,
    };
    unsafe {
        let me = HWND(raw as *mut core::ffi::c_void);

        /* 桌面窗口 —— 用 GetShellWindow 而不是 FindWindow("Progman")：
           开机早期或换了壁纸宿主（WorkerW）时前者更稳。 */
        let desk = GetShellWindow();
        if !desk.0.is_null() {
            /* EnumWindows 按 Z 序（上→下）枚举顶层窗口 */
            let mut list: Vec<isize> = Vec::with_capacity(512);
            let ok = EnumWindows(Some(enum_z), LPARAM(&mut list as *mut Vec<isize> as isize));
            if ok.is_ok() {
                if let Some(zp) = list.iter().position(|&x| x == desk.0 as isize) {
                    if zp > 0 {
                        /* 快路径：绝大多数时候桌面正上方那一格就能直接用。
                           Win+D 之后守护线程每 5ms 复查一次，这里越短越好。 */
                        let anchor = list[zp - 1];
                        if anchor == raw {
                            return false; /* 已经紧贴桌面 */
                        }
                        if SetWindowPos(
                            me,
                            HWND(anchor as *mut core::ffi::c_void),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                        .is_ok()
                        {
                            return true;
                        }

                        /* 慢路径：那一格是输入法壳窗（CicLoaderWndClass / MSCTFIME UI），
                           EnumWindows 枚举完之后它随时可能已经销毁 —— SetWindowPos 拿它
                           当 hwndInsertAfter 会直接返回 0，卡片沉不下去，停在所有窗口之上。
                           这就是「取消置顶后回不到桌面」的真正原因。
                           逐格往前退，跳过壳窗，最多试 24 格。 */
                        let mut tries = 0;
                        for k in (0..zp - 1).rev() {
                            let cand = list[k];
                            if cand == raw {
                                return false;
                            }
                            if is_shell_noise(cand) {
                                continue;
                            }
                            if SetWindowPos(
                                me,
                                HWND(cand as *mut core::ffi::c_void),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                            )
                            .is_ok()
                            {
                                return true;
                            }
                            tries += 1;
                            if tries >= 24 {
                                break;
                            }
                        }
                    }
                }
            }
        }

        /* 找不到桌面（Explorer 正在重启），或者桌面本身就在最上层：退回绝对底部 */
        let _ = SetWindowPos(
            me,
            HWND_BOTTOM,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    true
}

#[cfg(not(windows))]
fn bottom_at(_raw: isize) -> bool {
    false
}

#[cfg(windows)]
fn send_to_bottom(win: &WebviewWindow) {
    if let Ok(h) = win.hwnd() {
        bottom_at(h.0 as isize);
    }
}

#[cfg(not(windows))]
fn send_to_bottom(_win: &WebviewWindow) {}

/* ── 界面语言 ─────────────────────────────────────────────────
托盘菜单是系统级 UI，读不到 WebView 的 navigator.language，所以自己问一次
Windows：主语言 ID（低 10 位）= 0x04 就是中文（简/繁/港/新都算）。
前端在设置里选过语言后调 set_lang 把托盘一起刷新，两边不会打架。 */
#[cfg(windows)]
fn is_zh_current() -> bool {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;
    let id = unsafe { GetUserDefaultUILanguage() } as u32;
    (id & 0x3ff) == 0x04
}

#[cfg(not(windows))]
fn is_zh_current() -> bool {
    std::env::var("LANG").map(|v| v.starts_with("zh")).unwrap_or(false)
}

/* 托盘菜单：id 不变，只换显示文字。切换语言时整张重建再 set_menu，
   不逐个 set_text —— 那样漏一个就是中英混排。 */
fn tray_menu(app: &tauri::AppHandle, zh: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let l = |a: &'static str, b: &'static str| if zh { a } else { b };
    let show_i = MenuItemBuilder::with_id("show", l("显示清单", "Show list")).build(app)?;
    let pin_i = MenuItemBuilder::with_id("pin", l("置顶 / 贴回桌面", "Pin / unpin")).build(app)?;
    let set_i = MenuItemBuilder::with_id("settings", l("设置…", "Settings…")).build(app)?;
    let quit_i = MenuItemBuilder::with_id("quit", l("退出 Desk List", "Quit Desk List")).build(app)?;
    MenuBuilder::new(app)
        .items(&[&show_i, &pin_i])
        .separator()
        .items(&[&set_i])
        .separator()
        .items(&[&quit_i])
        .build()
}

/* ── 真·置顶 ─────────────────────────────────────────────────
   Tauri 的 set_always_on_top 靠不住：置顶/贴回桌面这一下，窗口的 exstyle
   会从 0xC0130（layered + 穿透）整份改写成 0x40110，WS_EX_TOPMOST（0x8）
   在这一步丢掉；而且改写是异步落到消息循环里的，谁先谁后没准。

   实测现象（这也是最容易误判的地方）：
     · 切到置顶态的那一瞬间，SetWindowPos(HWND_TOPMOST) 返回成功，
       紧接着读回来却还是 0x40110 —— 样式正在重建，这一位挂不上；
     · 同一个窗口等它稳定下来再设，就稳了；
     · 但鼠标一移上来，apply_mouse 又会跑一遍 set_ignore_cursor_events，
       照样把 0x8 冲掉。

   所以这里不做「一次性设置」的假设：
     1) 设之前先看，已经是目标状态就不动（省掉无谓的 Z 序重排）；
     2) 设完立刻验，不对就退到直接写 exstyle；
     3) 轮询线程每轮顺手看一眼，置顶态下发现 0x8 没了就补回来。
   一次 GetWindowLongW 是纳秒级，不担心开销。 */
#[cfg(windows)]
fn force_top_raw(hwnd: isize, on: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongPtrW, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WINDOW_LONG_PTR_INDEX,
    };
    unsafe {
        let h = HWND(hwnd as *mut _);
        let read = || GetWindowLongW(h, WINDOW_LONG_PTR_INDEX(-20)) as u32;
        if ((read() & 0x8) != 0) == on {
            return;
        }
        for _ in 0..3 {
            let _ = SetWindowPos(
                h,
                if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let now = read();
            if ((now & 0x8) != 0) == on {
                return;
            }
            /* 退路：直接写那一位。样式重建期间 SetWindowPos 会被吞掉。 */
            let want = if on { now | 0x8 } else { now & !0x8 };
            SetWindowLongPtrW(h, WINDOW_LONG_PTR_INDEX(-20), want as isize);
            if ((read() & 0x8) != 0) == on {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        logln(&format!("force_top({}) 三次都没挂上，ex=0x{:X}", on, read()));
    }
}

#[cfg(not(windows))]
fn force_top_raw(_hwnd: isize, _on: bool) {}

#[cfg(windows)]
fn force_top(win: &WebviewWindow, on: bool) {
    if let Ok(h) = win.hwnd() {
        force_top_raw(h.0 as isize, on);
    }
}

#[cfg(not(windows))]
fn force_top(_win: &WebviewWindow, _on: bool) {}

/* ── 抢在桌面被抬起来之后立刻坐回去 ───────────────────────────
   界面「贴在桌面上」= 我们的窗口紧贴在桌面窗口上面一格。
   Win+D / 点任务栏「显示桌面」实际是「把桌面窗口抬到所有窗口之上」，
   我们就被压在桌面背后了。只靠 0.55 秒一次的轮询去捞，用户看到的是
   「先消失一下、再闪回来」。

   实测：系统在桌面被抬起的前后 ~1ms 内会发 Z 序重排事件，但事件有时
   比真正的 Z 变化早零点几毫秒 —— 只听一次，正好会踩空。
   所以收到事件后开一个 160ms 的「守护窗口」：每 5ms 复查自己是不是
   还在桌面之上，不在就立刻插回去。错过最多 1 帧，肉眼看不出来，
   等于让 Win+D 对它完全无效。没有事件时线程彻底阻塞，不耗 CPU。

   —— 但 Z 序事件不只 Win+D 会发：点任务栏、弹菜单、切前台都会发。
      所以守护窗口是「自适应」的：前 4 次复查（20ms）如果一直好好地贴在
      桌面上，说明这次事件跟桌面无关，立刻收工，不为噪声付 160ms 的钱。
      一旦真的发现自己被盖（救过一次场），就老老实实守满 160ms。 */
#[cfg(windows)]
fn uptime_ms() -> u64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/* 事件回调只负责「踢一脚」，真正的活在这个线程里干 ——
   回调跑在钩子线程上，不能在里面做 EnumWindows / SetWindowPos 这种重活 */
#[cfg(windows)]
fn guard_poke() {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Mutex, OnceLock};
    static TX: OnceLock<Mutex<Sender<()>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel();
        guard_loop(rx);
        Mutex::new(tx)
    });
    if let Ok(t) = tx.lock() {
        let _ = t.send(());
    }
}

#[cfg(windows)]
fn guard_loop(rx: std::sync::mpsc::Receiver<()>) {
    thread::spawn(move || loop {
        /* 没有事件时永远等在这儿（零开销） */
        if rx.recv().is_err() {
            return;
        }
        let deadline = uptime_ms() + 160;
        /* 这次事件有没有真的把我们盖住过（救过场）。
           没救过 = 纯噪声，短守即可；救过 = 守满，别让 Win+D 钻空子。 */
        let mut rescued = false;
        let mut quiet = 0u32;
        loop {
            let me = MY_HWND.load(Ordering::Relaxed);
            /* 正在被操作（拖动 / 拉大小 / 输入文字）时窗口是前台、或者鼠标按着，
               这时候不能把它压回去，否则手还没松开卡片就沉到底下了。
               Win+D 用的是键盘，不受影响。 */
            let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            let held = unsafe {
                (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x01) as u16 & 0x8000)
                    != 0
            };
            let busy = fg.0 as isize == me || held;
            if me != 0
                && !busy
                && !PINNED.load(Ordering::Relaxed)
                && !SETTINGS.load(Ordering::Relaxed)
                && !NUDGE.load(Ordering::Relaxed)
            {
                /* 先花几百纳秒看「我下面紧邻的是不是桌面」。
                   系统里的 Z 序事件远比想象中密（浏览器切标签、托盘动画、
                   任何程序开合窗口都会发），要是每次都跑整套 EnumWindows，
                   光这个守护就够烧掉 1% CPU。绝大多数事件跟桌面无关，这里就挡掉了。 */
                if sits_on_desktop(me) {
                    quiet += 1;
                } else if bottom_at(me) {
                    rescued = true;
                    quiet = 0;
                } else {
                    quiet += 1;
                }
            }
            /* 前 4 次（≈20ms）一直稳稳贴在桌面上 —— 这次事件跟桌面无关，收工。
               20ms 已远超那 0.3ms 的事件提前量，不会漏掉真正的「桌面被抬起」。 */
            if !rescued && quiet >= 4 {
                break;
            }
            /* 守护期内又来事件就顺手吞掉，免得堆一串 */
            while rx.try_recv().is_ok() {}
            let now = uptime_ms();
            if now >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis((deadline - now).min(5)));
        }
    });
}

#[cfg(windows)]
unsafe extern "system" fn on_zorder(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _ev: u32,
    _hwnd: windows::Win32::Foundation::HWND,
    _idobj: i32,
    _idchild: i32,
    _tid: u32,
    _time: u32,
) {
    /* 置顶 / 设置面板本来就该在最上层，不参与这件事 */
    if PINNED.load(Ordering::Relaxed)
        || SETTINGS.load(Ordering::Relaxed)
        || NUDGE.load(Ordering::Relaxed)
    {
        return;
    }
    guard_poke();
}

/* 窗口矩形（物理像素）—— 直接问 Win32，不走 Tauri 的跨进程查询。
   轮询线程每 55ms 都要一次，用 Tauri API 一次要几十微秒，这里只要几百纳秒。 */
#[cfg(windows)]
fn win_rect(me: isize) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut r = RECT::default();
    unsafe {
        GetWindowRect(HWND(me as *mut core::ffi::c_void), &mut r).ok()?;
    }
    Some((r.left, r.top, r.right, r.bottom))
}

#[cfg(not(windows))]
fn win_rect(_me: isize) -> Option<(i32, i32, i32, i32)> {
    None
}

/* 「我下面紧邻的那一格就是桌面」—— 一次 GetWindow + GetClassName，几百纳秒。
   兜底校对每 0.16s 跑一次，真正要做的是 EnumWindows 整个顶层窗口列表，
   那是百微秒级。绝大多数时候我们本来就好好地贴在桌面上，用它先挡一道，
   只有它说「不在位」时才值得去枚举。

   注意：卡片和桌面之间永远夹着输入法的壳窗口（MSCTFIME UI / IME），
   它们没有尺寸、点不到、只是系统把 IME 挂在 Z 序里。所以必须能跨过去。 */
#[cfg(windows)]
fn sits_on_desktop(me: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindow, GW_HWNDNEXT};
    unsafe {
        let mut cur = HWND(me as *mut core::ffi::c_void);
        for _ in 0..8 {
            let Ok(next) = GetWindow(cur, GW_HWNDNEXT) else {
                return false;
            };
            if next.0.is_null() {
                return false;
            }
            let mut buf = [0u16; 64];
            let n = GetClassNameW(next, &mut buf);
            if n <= 0 {
                return false;
            }
            let cls = String::from_utf16_lossy(&buf[..n as usize]);
            match cls.as_str() {
                /* 壁纸宿主可能是 Progman，也可能被搬进某个 WorkerW */
                "Progman" | "WorkerW" => return true,
                /* 输入法壳窗：系统挂在这儿但看不见，跨过去继续看 */
                "MSCTFIME UI" | "IME" | "CicLoaderWndClass" | "Windows.UI.Core.CoreWindow" => {
                    cur = next;
                }
                /* 中间夹着真实的窗口，说明已经被隔开了 */
                _ => return false,
            }
        }
        false
    }
}

#[cfg(not(windows))]
fn sits_on_desktop(_me: isize) -> bool {
    true
}

/* 指针是否落在窗口内（四周留 10px，别让阴影区把窗口唤醒） */
#[cfg(windows)]
fn cursor_inside(l: i32, t: i32, r: i32, b: i32) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return false;
    }
    const INSET: i32 = 10;
    p.x >= l + INSET && p.x <= r - INSET && p.y >= t + INSET && p.y <= b - INSET
}

#[cfg(not(windows))]
fn cursor_inside(_l: i32, _t: i32, _r: i32, _b: i32) -> bool {
    false
}

/* ── 贴合态滚轮 ───────────────────────────────────────────────
   窗口在贴合态没有键盘焦点，WM_MOUSEWHEEL 会被前台窗口吃掉，滚不动。
   这里挂一个低层鼠标钩子：指针落在卡片上、且清单确实溢出时，把滚轮截下来
   累加到一个原子量，再由轮询线程发给前端去滚。不抢焦点，也不影响别的程序。 */
#[cfg(windows)]
static WHEEL_ACC: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(windows)]
static WHEEL_ON: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static RECT_CACHE: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/* 非 Windows 也用同名的空实现，轮询线程里不用分平台写两遍 */
#[cfg(not(windows))]
static WHEEL_ACC: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(not(windows))]
static WHEEL_ON: AtomicBool = AtomicBool::new(false);
#[cfg(not(windows))]
static RECT_CACHE: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/* ── 滚轮到底该滚谁 ─────────────────────────────────────────
   钩子只负责「截下来」，滚清单还是滚 Tab、还是滚抽屉，交给前端判断：
   把事件发生那一刻的光标点一起送过去（换算成客户区 CSS 像素），
   前端 elementFromPoint 一测就知道指针压在哪个可滚容器上。
   这样不用一个区域一个区域地往回报矩形，也不会出现「两套坐标系对不齐」。 */
static EVT_X: AtomicI32 = AtomicI32::new(0);
static EVT_Y: AtomicI32 = AtomicI32::new(0);

/* 清单没溢出、但 Tab 条溢出 / 抽屉开着的时候，滚轮也值得截下来。
   单独一个开关，别去碰 SCROLLABLE —— 那个还管着「悬停要不要接管鼠标」，
   一旦被 Tab 条的溢出顶起来，卡片就会挡住桌面上的点击。 */
static WHEEL_OK: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn inside_cache(x: i32, y: i32) -> bool {
    match RECT_CACHE.lock() {
        Ok(g) => match *g {
            Some((l, t, r, b)) => x >= l && x <= r && y >= t && y <= b,
            None => false,
        },
        Err(_) => false,
    }
}

/* 卡片在这个点上是不是被别的窗口盖住了。
   窗口沉到桌面之后，指针「压在卡片坐标上」并不等于用户看得到卡片 ——
   底下是浏览器/资源管理器时，滚轮必须留给它们，不能被我们吞掉。
   从 Z 序最上层往下走，走到自己为止：自己之上有可见窗口盖住就算被遮。 */
static MY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/* 显示器缩放比例，只查一次就记住：窗口拖到别的显示器（DPI 变了）很少见，
   真遇到重启一次也就好了 —— 为了它每 55ms 问一次 WebView2 不值得。 */
static SCALE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

/* 卡片在当前这个点上「是不是真的被别的程序挡住了」。
   以前是沿着 Z 序从顶往下走、谁先盖住这个点就算被遮 —— 这条路太脆：
   桌面本身（Progman）、任务栏、Windows 的手写浮层（ShellHandwritingCanvas）、
   输入体验的 CoreWindow 全是铺满全屏的窗口，随便哪一个都会让判断恒为「被遮挡」，
   于是滚轮钩子一次都不触发（桌面上的卡片滚不动，全是这个原因）。
   现在直接问系统：「这个点上的滚轮会落到哪个窗口？」
   拿到的如果不是我们（也不是桌面/任务栏/浮层这类根本收不到滚轮的壳窗口），
   才算被挡住 —— 鼠标穿透的浮层系统本来就会跳过，不用自己猜。 */
#[cfg(windows)]
fn covered_at(x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::{GetLastError, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClassNameW, GetWindowLongW, WindowFromPoint, GA_ROOT, GWL_EXSTYLE,
        WS_EX_TRANSPARENT,
    };
    let me = MY_HWND.load(Ordering::Relaxed);
    if me == 0 {
        return false;
    }
    let h = unsafe { WindowFromPoint(POINT { x, y }) };
    if h.0.is_null() {
        return false;
    }
    let root = unsafe { GetAncestor(h, GA_ROOT) };
    let root = if root.0.is_null() { h } else { root };
    if root.0 as isize == me {
        return false;
    }
    /* 这些窗口即便「盖」在卡片上，也不会收走滚轮，等于没盖 */
    let mut buf = [0u16; 96];
    let n = unsafe { GetClassNameW(root, &mut buf) };
    let name = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if matches!(
        name.as_str(),
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "ShellHandwritingCanvas"
            | "Windows.UI.Core.CoreWindow"
            | "ForegroundStaging"
    ) {
        return false;
    }
    let ex = unsafe { GetWindowLongW(root, GWL_EXSTYLE) } as u32;
    let _ = unsafe { GetLastError() };
    ex & WS_EX_TRANSPARENT.0 == 0
}

#[cfg(windows)]
unsafe extern "system" fn mouse_hook(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, MSLLHOOKSTRUCT, WM_MOUSEWHEEL,
    };
    if code >= 0 && wparam.0 as u32 == WM_MOUSEWHEEL && WHEEL_ON.load(Ordering::Relaxed) {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if inside_cache(info.pt.x, info.pt.y) && !covered_at(info.pt.x, info.pt.y) {
            let delta = ((info.mouseData >> 16) & 0xFFFF) as u16 as i16 as i32;
            /* 记下这一下是在哪儿滚的：前端要拿它做命中判断 */
            EVT_X.store(info.pt.x, Ordering::Relaxed);
            EVT_Y.store(info.pt.y, Ordering::Relaxed);
            WHEEL_ACC.fetch_add(delta, Ordering::Relaxed);
            return windows::Win32::Foundation::LRESULT(1); /* 吞掉，别让后面的窗口跟着滚 */
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn do_pin(win: &WebviewWindow, pinned: bool) {
    logln(&format!("do_pin({})", pinned));
    PINNED.store(pinned, Ordering::Relaxed);
    /* 用户显式改过状态：临时的「打字置顶」标记作废 */
    TEMP_PIN.store(false, Ordering::Relaxed);
    if pinned {
        /* 置顶态不需要知道 Ctrl，直接复位，避免残留 */
        CTRL.store(false, Ordering::Relaxed);
        SUPPRESS.store(false, Ordering::Relaxed);
    } else {
        /* 回到贴合态：上闩，直到 Ctrl 真的松开为止 */
        SUPPRESS.store(true, Ordering::Relaxed);
    }
    let _ = win.set_always_on_top(pinned);
    /* Tauri 这一下不可靠（见 force_top 注释），自己再确认一次 */
    force_top(win, pinned);
    apply_mouse(win);
    if pinned {
        /* 置顶也要补刀，而且必须等一会儿：
           切进置顶态的那一百多毫秒里，Tauri 正在把窗口从 layered 样式重新
           建成普通窗口（exstyle 从 0xC0130 变成 0x40110），这段时间里
           SetWindowPos(HWND_TOPMOST) 返回成功但 0x8 位根本挂不上 ——
           实测连补 5 次、每次隔 2ms，读回来还是 0x40110。
           等样式重建结束再补，一次就成。 */
        let w2 = win.clone();
        thread::spawn(move || {
            for step in [120u64, 220] {
                thread::sleep(Duration::from_millis(step));
                if !PINNED.load(Ordering::Relaxed) || SETTINGS.load(Ordering::Relaxed) {
                    return;
                }
                force_top(&w2, true);
            }
        });
    }
    if !pinned {
        /* 沉底要压两轮：
           第一轮是正常的「立刻贴回桌面」；
           但 set_ignore_cursor_events 真正改 exstyle 的时机比这一行晚 ——
           Windows 重建 layered 样式时会顺手把窗口顶到最前，
           于是第一轮白沉，用户看到的就是「取消了置顶却浮在所有窗口之上」。
           所以等它改完再补一轮（160ms），再远一点（380ms）兜一次底。 */
        send_to_bottom(win);
        let w2 = win.clone();
        thread::spawn(move || {
            for step in [160u64, 220] {
                thread::sleep(Duration::from_millis(step));
                if PINNED.load(Ordering::Relaxed)
                    || SETTINGS.load(Ordering::Relaxed)
                    || NUDGE.load(Ordering::Relaxed)
                    || TYPING.load(Ordering::Relaxed)
                {
                    return;
                }
                send_to_bottom(&w2);
            }
        });
    }
    let _ = win.emit("pin", pinned);
    /* 关键：把 ctrl 状态一并推给前端。
       上一版只改了 Rust 侧的原子量、没通知前端，于是「按住 Ctrl 的同时按
       Ctrl+Alt+D 回到贴合态」会让前端 body.ctrl 永久停在 true，
       画面卡在半透明的激活态——就是你说的那个「留在中间界面」。 */
    let _ = win.emit("ctrl", false);
}

/* ── 命令 ─────────────────────────────────────────────────── */
#[tauri::command]
fn set_pin(window: WebviewWindow, pinned: bool) {
    do_pin(&window, pinned);
}

#[tauri::command]
fn toggle_pin(window: WebviewWindow) {
    let next = !PINNED.load(Ordering::Relaxed);
    do_pin(&window, next);
}

#[tauri::command]
fn set_typing(window: WebviewWindow, on: bool) {
    if TYPING.load(Ordering::Relaxed) == on {
        return;
    }
    TYPING.store(on, Ordering::Relaxed);
    /* 打字期间系统会把窗口提到普通窗口最上层（输入框要拿键盘焦点）。
       一结束就沉回去，否则贴合态会一直盖着后面的窗口。 */
    if !on && !SETTINGS.load(Ordering::Relaxed) {
        if TEMP_PIN.swap(false, Ordering::Relaxed) {
            /* 是 Ctrl+Alt+N 顶上来的，整条退回贴合态 */
            do_pin(&window, false);
        } else if !PINNED.load(Ordering::Relaxed) {
            send_to_bottom(&window);
        }
    }
    apply_mouse(&window);
}

/* 前端告诉我们「清单现在溢出了」。
   只有溢出的时候，悬停才值得接管鼠标 —— 否则穿透照旧。 */
#[tauri::command]
fn set_scrollable(window: WebviewWindow, on: bool) {
    if SCROLLABLE.load(Ordering::Relaxed) == on {
        return;
    }
    SCROLLABLE.store(on, Ordering::Relaxed);
    apply_mouse(&window);
}

#[tauri::command]
fn set_ignore(window: WebviewWindow, ignore: bool) {
    let _ = window.set_ignore_cursor_events(ignore);
}

/* 前端把「除了清单之外还有没有得滚」（Tab 条溢出 / 抽屉展开）单独报上来。
   和 set_scrollable 分开：那个还会改「悬停要不要接管鼠标」，不能被这条顶起来。 */
#[tauri::command]
fn set_wheel_ok(on: bool) {
    WHEEL_OK.store(on, Ordering::Relaxed);
}

/* 尺寸统一走一个命令：宽高一起提交，少一次 IPC、少一次 DWM 合成 */
#[tauri::command]
fn set_size(window: WebviewWindow, w: Option<f64>, h: Option<f64>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let cur = window.inner_size().unwrap_or_default();
    let mut cw = cur.width as f64 / scale;
    let mut ch = cur.height as f64 / scale;
    if let Some(v) = w {
        cw = v.clamp(MIN_W, MAX_W);
    }
    if let Some(v) = h {
        ch = v.clamp(MIN_H, MAX_H);
    }
    if (cw - cur.width as f64 / scale).abs() < 0.5 && (ch - cur.height as f64 / scale).abs() < 0.5 {
        return;
    }
    let _ = window.set_size(tauri::LogicalSize::new(cw, ch));
}

/* 设置面板并排贴在待办卡旁边，不遮挡清单、也不推动清单。
   所以窗口只朝「面板那一侧」长宽，卡片在屏幕上的坐标保持原样：
     · 右侧放得下 → 窗口往右长
     · 右侧放不下、左侧放得下 → 窗口往左长（同时把 x 左移同样的量，卡片才不动）
   宽高、位置全由前端算好传进来，这里只负责落地。 */
const PANEL_GAP: f64 = 12.0;
const PANEL_W: f64 = 340.0;

/* 面板该放哪一侧、能做多宽 —— 纯查询，不改任何窗口状态。
   前端先问这一句，把布局切好（面板此刻还没显示），再去改窗口尺寸，
   这样不会出现「先长宽、再换边」的跳动。 */
#[tauri::command]
fn settings_side(window: WebviewWindow) -> serde_json::Value {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (mut x, mut w) = (0.0, 420.0);
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
        x = pos.x as f64 / scale;
        w = size.width as f64 / scale;
    }
    let (mut room_r, mut room_l) = (4000.0, 4000.0);
    if let Ok(Some(mon)) = window.current_monitor() {
        let sf = mon.scale_factor();
        let mx = mon.position().x as f64 / sf;
        let mw = mon.size().width as f64 / sf;
        room_r = (mx + mw - 8.0) - (x + w);
        room_l = x - (mx + 8.0);
    }
    let need = PANEL_W + PANEL_GAP;
    let (side, pw) = if room_r >= need {
        ("right", PANEL_W)
    } else if room_l >= need {
        ("left", PANEL_W)
    } else if room_r >= room_l {
        ("right", (room_r - PANEL_GAP).clamp(170.0, PANEL_W))
    } else {
        ("left", (room_l - PANEL_GAP).clamp(170.0, PANEL_W))
    };
    serde_json::json!({ "side": side, "pw": pw.round() })
}

#[tauri::command]
fn enter_settings(window: WebviewWindow, side: Option<String>, pw: Option<f64>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let mut x = 0.0;
    let mut y = 0.0;
    let mut w = 420.0;
    let mut h = 600.0;
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
        x = pos.x as f64 / scale;
        y = pos.y as f64 / scale;
        w = size.width as f64 / scale;
        h = size.height as f64 / scale;
    }
    if let Ok(mut p) = PREV.lock() {
        *p = Some((x, y, w, h));
    }

    SETTINGS.store(true, Ordering::Relaxed);
    let _ = window.set_always_on_top(true);
    let _ = window.set_ignore_cursor_events(false);

    let pw = pw.unwrap_or(340.0).clamp(160.0, 420.0);
    let mut grow = PANEL_GAP + pw;
    let to_left = side.as_deref() == Some("left");
    let mut nx = if to_left { x - grow } else { x };

    if let Ok(Some(mon)) = window.current_monitor() {
        let sf = mon.scale_factor();
        let mx = mon.position().x as f64 / sf;
        let min_x = mx + 8.0;
        /* 左伸越界：把窗口推回屏幕内，宽度相应缩回来（面板自己会窄一点） */
        if nx < min_x {
            let cut = min_x - nx;
            nx = min_x;
            grow = (grow - cut).max(160.0);
        }
    }

    let _ = window.set_size(tauri::LogicalSize::new(w + grow, h));
    let _ = window.set_position(tauri::LogicalPosition::new(nx, y));
    let _ = window.set_focus();
}

/* 高度按内容自适应：宽度不动，位置也绝不动 ——
   只会往下长；下沿超出屏幕就压回来，宁可面板内部滚动，也不把卡片顶上去。 */
#[tauri::command]
fn fit_settings(window: WebviewWindow, h: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let mut hh = h.clamp(320.0, 1400.0);
    let mut w = window
        .outer_size()
        .map(|s| s.width as f64 / scale)
        .unwrap_or(760.0);
    let (mut x, mut y) = window
        .outer_position()
        .map(|p| (p.x as f64 / scale, p.y as f64 / scale))
        .unwrap_or((0.0, 0.0));

    /* 夹边界要用「工作区」，不是「整块屏幕」。
       任务栏占着底部 40~48px，按屏幕高度去夹，面板就会长长地压到任务栏上；
       而设置态窗口是置顶、且不穿透的 —— 压在任务栏上的那一条会把鼠标全吃掉：
       任务栏点不动、右下角托盘图标点不动、右键也没反应。
       顺带把左右边界也按工作区算，侧边任务栏 / 停靠栏的机器一样安全。 */
    if let Ok(Some(mon)) = window.current_monitor() {
        let sf = mon.scale_factor();
        let wa = mon.work_area();
        let wl = wa.position.x as f64 / sf + 8.0;
        let wt = wa.position.y as f64 / sf + 8.0;
        let wr = (wa.position.x as f64 + wa.size.width as f64) / sf - 8.0;
        let wb = (wa.position.y as f64 + wa.size.height as f64) / sf - 8.0;

        if y + hh > wb {
            hh = (wb - y).max(300.0);
        }
        /* 卡片本来就贴着屏幕下沿：光缩高度不够，整块往上挪 */
        if y + hh > wb {
            y = (wb - hh).max(wt);
        }
        if x + w > wr {
            w = (wr - x).max(360.0);
        }
        if x < wl {
            x = wl;
        }
        let _ = window.set_position(tauri::LogicalPosition::new(x, y));
    }
    let _ = window.set_size(tauri::LogicalSize::new(w, hh));
}

/* 版本号只维护一处：settings 页脚从 Rust 取，不再手写在前端（上一版漏改过）。 */
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn exit_settings(window: WebviewWindow) {
    SETTINGS.store(false, Ordering::Relaxed);
    let prev = PREV.lock().ok().and_then(|g| *g);
    if let Some((x, y, w, h)) = prev {
        let _ = window.set_size(tauri::LogicalSize::new(w, h));
        let _ = window.set_position(tauri::LogicalPosition::new(x, y));
    }
    let _ = window.set_always_on_top(PINNED.load(Ordering::Relaxed));
    if !PINNED.load(Ordering::Relaxed) {
        send_to_bottom(&window);
    }
    apply_mouse(&window);
}

/* 开机自启：Windows 走 HKCU\...\Run，macOS 走 LaunchAgent */
#[tauri::command]
fn set_autostart(on: bool) -> bool {
    autostart_impl(on)
}

#[cfg(windows)]
fn autostart_impl(on: bool) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().unwrap_or_default();
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let out = if on {
        std::process::Command::new("reg")
            .args([
                "add",
                key,
                "/v",
                "DeskList",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", exe.display()),
                "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    } else {
        std::process::Command::new("reg")
            .args(["delete", key, "/v", "DeskList", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    };
    matches!(out, Ok(o) if o.status.success())
}

#[cfg(target_os = "macos")]
fn autostart_impl(on: bool) -> bool {
    let home = match std::env::var_os("HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => return false,
    };
    let plist = home.join("Library/LaunchAgents/com.workbuddy.desklist.plist");
    if on {
        let exe = std::env::current_exe().unwrap_or_default();
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.workbuddy.desklist</string>
<key>ProgramArguments</key><array><string>{}</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#,
            exe.display()
        );
        let _ = std::fs::create_dir_all(plist.parent().unwrap_or(&home));
        std::fs::write(&plist, body).is_ok()
    } else {
        std::fs::remove_file(&plist).is_ok()
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn autostart_impl(_on: bool) -> bool {
    false
}

#[tauri::command]
fn autostart_state() -> bool {
    autostart_query()
}

#[cfg(windows)]
fn autostart_query() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "DeskList",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    matches!(out, Ok(o) if o.status.success())
}

#[cfg(target_os = "macos")]
fn autostart_query() -> bool {
    std::env::var_os("HOME")
        .map(|h| {
            std::path::PathBuf::from(h)
                .join("Library/LaunchAgents/com.workbuddy.desklist.plist")
                .exists()
        })
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn autostart_query() -> bool {
    false
}

#[tauri::command]
fn set_lang(app: tauri::AppHandle, lang: String) {
    let zh = match lang.as_str() {
        "zh" => true,
        "en" => false,
        _ => is_zh_current(),
    };
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(m) = tray_menu(&app, zh) {
            let _ = tray.set_menu(Some(m));
        }
    }
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn hide_window(window: WebviewWindow) {
    let _ = window.hide();
}

/* 前端报错落到日志里，省得每次都开 devtools */
#[tauri::command]
fn dbg(msg: String) {
    logln(&format!("JS {}", msg));
}

/* ── 快捷键：可由前端改绑 ───────────────────────────────────
   前端传 {mods:["Ctrl","Alt"], code:"KeyD"}，这里翻译成 Shortcut。
   Code 的取值跟浏览器 KeyboardEvent.code 对齐，省得两边各写一套。 */
fn code_of(s: &str) -> Option<Code> {
    if let Some(rest) = s.strip_prefix("Key") {
        let mut it = rest.chars();
        let c = it.next()?;
        if it.next().is_some() {
            return None;
        }
        return Some(match c {
            'A' => Code::KeyA, 'B' => Code::KeyB, 'C' => Code::KeyC, 'D' => Code::KeyD,
            'E' => Code::KeyE, 'F' => Code::KeyF, 'G' => Code::KeyG, 'H' => Code::KeyH,
            'I' => Code::KeyI, 'J' => Code::KeyJ, 'K' => Code::KeyK, 'L' => Code::KeyL,
            'M' => Code::KeyM, 'N' => Code::KeyN, 'O' => Code::KeyO, 'P' => Code::KeyP,
            'Q' => Code::KeyQ, 'R' => Code::KeyR, 'S' => Code::KeyS, 'T' => Code::KeyT,
            'U' => Code::KeyU, 'V' => Code::KeyV, 'W' => Code::KeyW, 'X' => Code::KeyX,
            'Y' => Code::KeyY, 'Z' => Code::KeyZ,
            _ => return None,
        });
    }
    if let Some(rest) = s.strip_prefix("Digit") {
        return Some(match rest {
            "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2,
            "3" => Code::Digit3, "4" => Code::Digit4, "5" => Code::Digit5,
            "6" => Code::Digit6, "7" => Code::Digit7, "8" => Code::Digit8,
            "9" => Code::Digit9,
            _ => return None,
        });
    }
    Some(match s {
        "F1" => Code::F1, "F2" => Code::F2, "F3" => Code::F3, "F4" => Code::F4,
        "F5" => Code::F5, "F6" => Code::F6, "F7" => Code::F7, "F8" => Code::F8,
        "F9" => Code::F9, "F10" => Code::F10, "F11" => Code::F11, "F12" => Code::F12,
        "Space" => Code::Space,
        "Backquote" => Code::Backquote,
        "ArrowUp" => Code::ArrowUp, "ArrowDown" => Code::ArrowDown,
        "ArrowLeft" => Code::ArrowLeft, "ArrowRight" => Code::ArrowRight,
        "Comma" => Code::Comma, "Period" => Code::Period, "Slash" => Code::Slash,
        "Semicolon" => Code::Semicolon, "Quote" => Code::Quote,
        "BracketLeft" => Code::BracketLeft, "BracketRight" => Code::BracketRight,
        "Minus" => Code::Minus, "Equal" => Code::Equal,
        _ => return None,
    })
}

fn shortcut_of(v: &serde_json::Value) -> Option<Shortcut> {
    let code = code_of(v.get("code")?.as_str()?)?;
    let mut m = Modifiers::empty();
    if let Some(list) = v.get("mods").and_then(|x| x.as_array()) {
        for item in list {
            match item.as_str().unwrap_or("") {
                "Ctrl" => m |= Modifiers::CONTROL,
                "Alt" => m |= Modifiers::ALT,
                "Shift" => m |= Modifiers::SHIFT,
                "Win" => m |= Modifiers::SUPER,
                _ => {}
            }
        }
    }
    if m.is_empty() {
        return None;
    }
    Some(Shortcut::new(Some(m), code))
}

fn bind_pin(app: &tauri::AppHandle, sc: Shortcut) -> Result<(), String> {
    let h = app.clone();
    app.global_shortcut()
        .on_shortcut(sc, move |_a, _s, ev| {
            if ev.state() == ShortcutState::Pressed {
                if let Some(w) = h.get_webview_window("main") {
                    let next = !PINNED.load(Ordering::Relaxed);
                    do_pin(&w, next);
                }
            }
        })
        .map_err(|e| e.to_string())
}

fn bind_add(app: &tauri::AppHandle, sc: Shortcut) -> Result<(), String> {
    let h = app.clone();
    app.global_shortcut()
        .on_shortcut(sc, move |_a, _s, ev| {
            if ev.state() == ShortcutState::Pressed {
                if let Some(w) = h.get_webview_window("main") {
                    if !PINNED.load(Ordering::Relaxed) {
                        let _ = w.set_always_on_top(true);
                        PINNED.store(true, Ordering::Relaxed);
                        /* 只为了打字临时顶上来，输入结束要自己退回去 */
                        TEMP_PIN.store(true, Ordering::Relaxed);
                        apply_mouse(&w);
                        let _ = w.emit("pin", true);
                    }
                    /* 热键是从别的应用按下来的，窗口这时没有键盘焦点 ——
                       不给焦点的话，用户接着打的字全落到原来那个程序里 */
                    let _ = w.show();
                    let _ = w.set_focus();
                    let _ = w.emit("new-item", ());
                }
            }
        })
        .map_err(|e| e.to_string())
}

/* 改绑：先全部注销，再按新组合注册。
   返回 false 表示组合被别的程序占了，前端会提示换一个。 */
#[tauri::command]
fn set_shortcuts(
    app: tauri::AppHandle,
    pin: serde_json::Value,
    add: serde_json::Value,
) -> bool {
    let Some(p) = shortcut_of(&pin) else { return false };
    let Some(a) = shortcut_of(&add) else { return false };
    let _ = app.global_shortcut().unregister_all();
    if bind_pin(&app, p).is_err() {
        /* 回滚成默认，别让用户卡在「一个热键都没有」 */
        let _ = app.global_shortcut().unregister_all();
        let d = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyD);
        let n = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyN);
        let _ = bind_pin(&app, d);
        let _ = bind_add(&app, n);
        return false;
    }
    let _ = bind_add(&app, a);
    true
}

/* ── 单实例 ──────────────────────────────────────────────────
   多开一个的代价，比看起来大得多：
     · 两个进程读写同一份 data.json —— 谁后写谁盖掉前者，清单会「丢东西」；
     · 各注册一遍全局热键，第二套静默失败，热键从此时灵时不灵；
     · 各挂一个托盘图标，退出一个还剩一个幽灵图标；
     · 抢同一个 WebView2 用户数据目录，第二个经常直接白窗。
   所以第二个实例在创建任何窗口之前就退出。

   命名互斥体，名字带 `Local\` 前缀：按登录会话隔离，机器上另一个用户
   同时开着不受影响。句柄故意不关 —— 进程一退系统自动回收这个名字，
   不会留下「僵尸锁」把用户锁在门外。 */
static INSTANCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
fn claim_instance() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\DeskList.SingleInstance\0".encode_utf16().collect();
    let r = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) };
    /* 必须紧挨着读：ERROR_ALREADY_EXISTS 正是「已经有一个在跑」 */
    let err = unsafe { GetLastError() };
    match r {
        Ok(h) => {
            INSTANCE.store(h.0 as isize, Ordering::Relaxed);
            err != ERROR_ALREADY_EXISTS
        }
        /* 建不出来就别硬拦：宁可多开一个，也不能让用户打不开 */
        Err(_) => true,
    }
}

#[cfg(not(windows))]
fn claim_instance() -> bool {
    true
}

/* 第二个实例的遗言：留个记号就走。
   已经活着的那个读到它，把卡片端到眼前 —— 否则「双击图标毫无反应」，
   用户只会以为点坏了。 */
fn nudge_file() -> std::path::PathBuf {
    data_dir().join("nudge")
}

#[cfg(windows)]
fn poke_running() {
    let _ = std::fs::write(nudge_file(), b"1");
}

#[cfg(not(windows))]
fn poke_running() {}

/* 贴合态平时沉在桌面，被浏览器盖住是常态。双击图标就把它端上来 1.4 秒，
   然后自己沉回去 —— 不改变置顶状态，也不抢输入焦点。 */
#[cfg(windows)]
fn check_nudge(app: &tauri::AppHandle) {
    let f = nudge_file();
    if !f.exists() {
        return;
    }
    let _ = std::fs::remove_file(&f);
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    /* 置顶态 / 设置面板本来就在最上层，什么都不用做 */
    if PINNED.load(Ordering::Relaxed) || SETTINGS.load(Ordering::Relaxed) {
        return;
    }
    let _ = w.set_always_on_top(true);
    NUDGE.store(true, Ordering::Relaxed);
    let w2 = w.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1400));
        NUDGE.store(false, Ordering::Relaxed);
        if !PINNED.load(Ordering::Relaxed) && !SETTINGS.load(Ordering::Relaxed) {
            let _ = w2.set_always_on_top(false);
            send_to_bottom(&w2);
        }
    });
}

#[cfg(not(windows))]
fn check_nudge(_app: &tauri::AppHandle) {}

/* ── 启动 ─────────────────────────────────────────────────── */
fn main() {
    /* 已经有实例在跑：打个招呼，然后什么窗口都不建，直接走 */
    if !claim_instance() {
        poke_running();
        std::process::exit(0);
    }
    /* 上一次留下的记号（比如上次退出太急）先清掉，免得一启动就自己弹一下 */
    let _ = std::fs::remove_file(nudge_file());

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            set_pin,
            toggle_pin,
            set_ignore,
            set_typing,
            set_scrollable,
            set_wheel_ok,
            set_size,
            settings_side,
            enter_settings,
            fit_settings,
            exit_settings,
            app_version,
            set_shortcuts,
            set_lang,
            set_autostart,
            autostart_state,
            quit_app,
            hide_window,
            load_data,
            save_data,
            dbg
        ])
        .on_window_event(|win, ev| match ev {
            tauri::WindowEvent::Focused(f) => {
                let _ = win.emit("focus", *f);
                /* 失焦后顺手沉底：新建待办时给过焦点、唤出过设置面板，
                   这些操作都会把窗口提到普通窗口最上层；一旦结束，
                   贴合态要立刻回到桌面，而不是等用户去点一下桌面。 */
                if !*f
                    && !PINNED.load(Ordering::Relaxed)
                    && !SETTINGS.load(Ordering::Relaxed)
                    && !NUDGE.load(Ordering::Relaxed)
                {
                    #[cfg(windows)]
                    if let Ok(h) = win.hwnd() {
                        bottom_at(h.0 as isize);
                    }
                }
            }
            tauri::WindowEvent::Moved(p) => {
                if SETTINGS.load(Ordering::Relaxed) {
                    return;
                }
                let sf = win.scale_factor().unwrap_or(1.0);
                if let Ok(mut m) = POS.lock() {
                    *m = Some((p.x as f64 / sf, p.y as f64 / sf));
                }
                DIRTY.store(true, Ordering::Relaxed);
            }
            _ => {}
        })
        .setup(|app| {
            let win = app.get_webview_window("main").expect("main window");

            /* 记下自己的 HWND：滚轮钩子要靠它判断「卡片是不是被别的窗口盖住」 */
            #[cfg(windows)]
            if let Ok(h) = win.hwnd() {
                MY_HWND.store(h.0 as isize, Ordering::Relaxed);
            }

            /* 位置：上次的位置优先，否则落到主屏右上角 */
            if let Some((x, y)) = load_pos() {
                let _ = win.set_position(tauri::LogicalPosition::new(x, y));
            } else if let Ok(Some(mon)) = win.primary_monitor() {
                let sf = mon.scale_factor();
                let size = mon.size();
                let logical_w = size.width as f64 / sf;
                let x = (logical_w - 400.0 - PAD * 2.0 - 40.0).max(8.0);
                let _ = win.set_position(tauri::LogicalPosition::new(x, 48.0));
            }

            /* 尺寸：先按上次手动调过的卡片尺寸摆好窗口，卡片才不会被夹小 */
            if let Some((cw, ch)) = saved_card_size() {
                let _ = win.set_size(tauri::LogicalSize::new(cw + PAD * 2.0, ch + PAD * 2.0));
            }

            let _ = win.set_always_on_top(PINNED.load(Ordering::Relaxed));
            if !PINNED.load(Ordering::Relaxed) {
                let w2 = win.clone();
                /* 等首帧画完再沉底，避免窗口还没上屏就被重排 */
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(120));
                    send_to_bottom(&w2);
                });
            }
            apply_mouse(&win);
            let _ = win.emit("pin", PINNED.load(Ordering::Relaxed));

            /* 位置落盘线程：拖动时 Moved 会高频触发，攒 1 秒再写一次 */
            thread::spawn(move || loop {
                thread::sleep(Duration::from_millis(1000));
                if DIRTY.swap(false, Ordering::Relaxed) {
                    if let Ok(g) = POS.lock() {
                        if let Some((x, y)) = *g {
                            save_pos(x, y);
                        }
                    }
                }
            });

            /* 低层鼠标钩子线程：必须有消息泵，否则钩子收不到回调 */
            #[cfg(windows)]
            thread::spawn(|| unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, MSG,
                    WH_MOUSE_LL,
                };
                let h = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0);
                if h.is_err() {
                    logln("鼠标钩子安装失败");
                    return;
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });

            /* Z 序重排事件线程
               ——— 桌面被抬起来（Win+D / 「显示桌面」）的那一刻就把卡片坐回它上面，
                   不靠轮询兜底，中间不留「被盖住」的窗口期
               ——— 和鼠标钩子一样，必须有消息泵，否则收不到回调 */
            #[cfg(windows)]
            thread::spawn(|| unsafe {
                use windows::Win32::UI::Accessibility::SetWinEventHook;
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, TranslateMessage, MSG, EVENT_OBJECT_REORDER,
                    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
                };
                for ev in [EVENT_OBJECT_REORDER, EVENT_SYSTEM_FOREGROUND] {
                    let _ = SetWinEventHook(ev, ev, None, Some(on_zorder), 0, 0, WINEVENT_OUTOFCONTEXT);
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });

            /* Ctrl + 悬停 轮询线程
               ——— 悬停：任何时刻都要知道指针在不在卡片上，用来决定「唤醒」与否
               ——— Ctrl：只在贴合态才真正干活，置顶态本来就常驻可点
               ——— 55ms 一次。窗口矩形 / 前台窗口 / 按键全部用 Win32 原生调用：
                   GetWindowRect / GetForegroundWindow / GetCursorPos / GetAsyncKeyState
                   都是百纳秒级。这里绝对不能用 Tauri 的 outer_position() 之类 ——
                   它们要跨进程问 WebView2，单次几十微秒 × 18Hz 就是白白烧掉 0.4% CPU。
            */
            let handle = app.handle().clone();
            thread::spawn(move || {
                let mut tick: u32 = 0;
                /* nudge 每 ~0.6s 看一次就够了，不必跟着 55ms 的节奏去摸磁盘 */
                let mut ntick: u32 = 0;
                loop {
                ntick = ntick.wrapping_add(1);
                if ntick % 11 == 0 {
                    check_nudge(&handle);
                }
                let pinned = PINNED.load(Ordering::Relaxed);
                let settings = SETTINGS.load(Ordering::Relaxed);

                /* 窗口句柄缓存住，整个循环只取一次，不再每轮去 HashMap 里翻 */
                let me = MY_HWND.load(Ordering::Relaxed);
                /* 置顶态兜底：鼠标挪动、托盘菜单、切前台都会让 apply_mouse 再跑一遍
                   set_ignore_cursor_events，而那一下会把 WS_EX_TOPMOST 整位冲掉。
                   每轮顺手看一眼（已经是置顶就直接返回，只有 GetWindowLongW），
                   掉了立刻补回来 —— 用户不会看到「置顶一会儿又被盖住」。 */
                if pinned && me != 0 {
                    force_top_raw(me, true);
                }
                let rect = if me != 0 { win_rect(me) } else { None };

                if let Some((l, t, r, b)) = rect {
                    /* 缓存窗口矩形，给鼠标钩子判区域用（钩子里不能调 Tauri API） */
                    if let Ok(mut g) = RECT_CACHE.lock() {
                        *g = Some((l, t, r, b));
                    }
                    /* 有没有键盘焦点：直接问前台窗口是谁，比自己是不是 focused 便宜得多 */
                    let focused = unsafe {
                        windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize
                            == me
                    };
                    /* 什么时候才需要我们自己截滚轮：
                       —— 贴合态（置顶态 / 设置面板本来就是普通窗口）
                       —— 清单溢出，或者 Tab 条 / 抽屉有得滚
                       —— 只要鼠标是穿透的，WebView 就一定收不到滚轮，只能我们接；
                          没穿透时再看焦点：有焦点让 WebView2 自己滚，
                          再截一道就成了「滚一格拉回一格」，永远停在原地。 */
                    WHEEL_ON.store(
                        !pinned
                            && !settings
                            && (IGNORING.load(Ordering::Relaxed) || !focused)
                            && (SCROLLABLE.load(Ordering::Relaxed)
                                || WHEEL_OK.load(Ordering::Relaxed)),
                        Ordering::Relaxed,
                    );
                    let acc = WHEEL_ACC.swap(0, Ordering::Relaxed);
                    if acc != 0 {
                        /* 把光标点换算成「客户区里的 CSS 像素」一起送过去：
                             getBoundingClientRect / elementFromPoint 认的就是这套坐标。
                           窗口没有标题栏，客户区原点 = 窗口原点 + 0，再除个缩放即可。
                           缩放值按显示器缓存，只有真的滚了才去查。 */
                        let s = SCALE.get_or_init(|| {
                            handle
                                .get_webview_window("main")
                                .and_then(|w| w.scale_factor().ok())
                                .unwrap_or(1.0)
                        });
                        let cx = (EVT_X.load(Ordering::Relaxed) - l) as f64 / *s;
                        let cy = (EVT_Y.load(Ordering::Relaxed) - t) as f64 / *s;
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.emit(
                                "wheel",
                                serde_json::json!({ "d": acc, "x": cx, "y": cy }),
                            );
                        }
                    }

                    if !pinned && !settings {
                        let hov = cursor_inside(l, t, r, b);
                        if hov != HOVER.load(Ordering::Relaxed) {
                            HOVER.store(hov, Ordering::Relaxed);
                            if let Some(w) = handle.get_webview_window("main") {
                                apply_mouse(&w);
                                let _ = w.emit("hover", hov);
                            }
                        }
                        /* 兜底：万一某个系统路径没发 Z 序事件（换主题、资源管理器重启），
                           每 ~0.16s 也自己校对一次，保证「贴在桌面」这件事不会失效。
                           先用几百纳秒的邻窗判断挡一道，真不在位才去做整套枚举。 */
                        tick += 1;
                        if tick % 3 == 0
                            && !focused
                            && !NUDGE.load(Ordering::Relaxed)
                            && !sits_on_desktop(me)
                        {
                            bottom_at(me);
                        }
                    } else if HOVER.swap(false, Ordering::Relaxed) {
                        /* 置顶/设置态不需要悬停语义，顺手清掉，免得回到贴合态时残留 */
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.emit("hover", false);
                        }
                    }
                }

                if pinned || settings {
                    thread::sleep(Duration::from_millis(180));
                    continue;
                }

                let raw = ctrl_down();
                /* 抑制期内：只看「有没有松开」，不对外广播 */
                if SUPPRESS.load(Ordering::Relaxed) {
                    if raw {
                        thread::sleep(Duration::from_millis(55));
                        continue;
                    }
                    SUPPRESS.store(false, Ordering::Relaxed);
                }
                if raw != CTRL.load(Ordering::Relaxed) {
                    CTRL.store(raw, Ordering::Relaxed);
                    if let Some(w) = handle.get_webview_window("main") {
                        apply_mouse(&w);
                        let _ = w.emit("ctrl", raw);
                    }
                }
                thread::sleep(Duration::from_millis(55));
                }
            });

            /* 全局热键：默认 Ctrl+Alt+D / Ctrl+Alt+N，之后前端可以用
               set_shortcuts 改绑（on_shortcut 内部同时完成注册 + 绑定处理器） */
            let sc_d = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyD);
            let sc_n = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyN);
            if let Err(e) = bind_pin(app.handle(), sc_d) {
                logln(&format!("Ctrl+Alt+D 注册失败: {}", e));
            }
            if let Err(e) = bind_add(app.handle(), sc_n) {
                logln(&format!("Ctrl+Alt+N 注册失败: {}", e));
            }

            /* 托盘 */
            let menu = tray_menu(app.handle(), is_zh_current())?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .tooltip("Desk List")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "pin" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let next = !PINNED.load(Ordering::Relaxed);
                            do_pin(&w, next);
                        }
                    }
                    "settings" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                            let _ = w.emit("open-settings", ());
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            /* 自动化验收用：带这个环境变量启动会直接打开设置面板 */
            if std::env::var_os("DESKLIST_OPEN_SETTINGS").is_some() {
                let h = app.handle().clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(1500));
                    if let Some(w) = h.get_webview_window("main") {
                        let _ = w.emit("open-settings", ());
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running DeskList");
}
