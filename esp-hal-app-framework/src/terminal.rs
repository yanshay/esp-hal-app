use core::cell::RefCell;

use alloc::vec::Vec;

pub static mut TERM: once_cell::unsync::OnceCell<Terminal> = once_cell::unsync::OnceCell::new();

pub struct Terminal {
    observers: Vec<alloc::rc::Weak<RefCell<dyn TerminalObserver>>>,
}

pub fn term() -> &'static Terminal {
    #[allow(static_mut_refs)]
    unsafe {
        TERM.get().expect("TERM not initialized")
    }
}

pub fn term_mut() -> &'static mut Terminal {
    #[allow(static_mut_refs)]
    unsafe {
        TERM.get_mut().expect("TERM not initialized")
    }
}

impl Terminal {
    pub fn initialize() {
        let global_term = Self {
            observers: Vec::new(),
        };
        unsafe {
            #[allow(static_mut_refs)]
            TERM.set(global_term).ok();
        }
    }
    pub fn add_text_new_line(&self, txt: &str) {
        self.notify_add_text("\n");
        self.notify_add_text(txt);
    }
    pub fn add_text_same_line(&self, txt: &str) {
        self.notify_add_text(txt);
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn TerminalObserver>>) {
        self.observers.push(observer);
    }
    pub fn notify_add_text(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow().on_add_text(text);
        }
    }
}

pub trait TerminalObserver {
    fn on_add_text(&self, text: &str);
}
