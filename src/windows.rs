use std::ptr;
use std::sync::mpsc::Sender;
use std::sync::{LazyLock, Mutex};
use winapi;
use winapi::um::processthreadsapi;
use winapi::um::winuser;

use crate::network::Event;

static mut MAIN_THREAD_ID: u32 = 0;

static KEY_HANDLER: LazyLock<Mutex<KeyHandler>> = LazyLock::new(|| {
    Mutex::new(KeyHandler::new(vec![
        winuser::VK_LCONTROL as u32,
        winuser::VK_RCONTROL as u32,
    ]))
});

pub struct KeyHandler {
    keycodes: Vec<u32>,
    down: Vec<u32>,
    active: bool,
}

impl KeyHandler {
    pub fn new(keycodes: Vec<u32>) -> KeyHandler {
        KeyHandler {
            keycodes,
            down: Vec::new(),
            active: false,
        }
    }

    pub fn install() -> bool {
        unsafe {
            MAIN_THREAD_ID = processthreadsapi::GetCurrentThreadId();

            let hook = winuser::SetWindowsHookExW(
                winuser::WH_KEYBOARD_LL,
                Some(hookproc),
                ptr::null_mut(),
                0,
            );
            !hook.is_null()
        }
    }

    pub fn pump() -> Option<u32> {
        let mut msg: winuser::MSG = unsafe { std::mem::zeroed() };

        unsafe {
            if winuser::GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                winuser::TranslateMessage(&msg);
                winuser::DispatchMessageW(&msg);
                Some(msg.message)
            } else {
                None
            }
        }
    }

    pub fn run(sender: Sender<Event>) {
        while let Some(msg) = KeyHandler::pump() {
            if msg == winuser::WM_NOTIFY {
                if let Ok(mut key_handler) = KEY_HANDLER.lock()
                    && key_handler.is_active()
                {
                    sender.send(Event::Activated).ok();
                }
            }
        }
    }

    pub fn is_active(&mut self) -> bool {
        let last = self.active;
        self.active = false;
        last
    }
}

unsafe extern "system" fn hookproc(code: i32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        if code == winuser::HC_ACTION {
            if let Ok(mut key_handler) = KEY_HANDLER.lock() {
                let kb = *(lparam as *const winuser::KBDLLHOOKSTRUCT);

                if wparam == winuser::WM_KEYDOWN as usize && !key_handler.down.contains(&kb.vkCode)
                {
                    key_handler.down.push(kb.vkCode);
                    key_handler.active |= key_handler
                        .keycodes
                        .iter()
                        .all(|k| key_handler.down.contains(&k));
                } else if wparam == winuser::WM_KEYUP as usize {
                    if let Some(index) = key_handler.down.iter().position(|k| *k == kb.vkCode) {
                        key_handler.down.remove(index);
                    }
                    if key_handler.down.is_empty() && key_handler.active {
                        winuser::PostThreadMessageW(MAIN_THREAD_ID, winuser::WM_NOTIFY, 0, 0);
                    }
                }
            }
        }
        winuser::CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
    }
}
