pub static SEPARATOR: char = '/';

#[macro_export]
macro_rules! file_name {
    () => {{
        const FULL: &str = file!();
        match FULL.rsplit_once($crate::log_ext::SEPARATOR) {
            Some((_, name)) => name,
            None => FULL,
        }
    }};
}

// Prints and returns the value of a given expression for quick and dirty
// debugging.
// implementation adapted from `std::dbg`
#[macro_export]
macro_rules! dbg {
    // NOTE: We cannot use `concat!` to make a static string as a format argument
    // of `eprintln!` because `file!` could contain a `{` or
    // `$val` expression could be a block (`{ .. }`), in which case the `println!`
    // will be malformed.
    () => {
        log::debug!("[{}:{}]", $crate::file_name!(), ::core::line!())
    };
    ($val:expr $(,)?) => {
        // Use of `match` here is intentional because it affects the lifetimes
        // of temporaries - https://stackoverflow.com/a/48732525/1063961
        match $val {
            tmp => {
                log::debug!("[{}:{}] {} = {:#?}",
                    $crate::file_name!(), ::core::line!(), ::core::stringify!($val), &tmp);
                tmp
            }
        }
    };
    ($($val:expr),+ $(,)?) => {
        ($($crate::dbg!($val)),+,)
    };
}

#[macro_export]
macro_rules! dbgt {
    // NOTE: We cannot use `concat!` to make a static string as a format argument
    // of `eprintln!` because `file!` could contain a `{` or
    // `$val` expression could be a block (`{ .. }`), in which case the `println!`
    // will be malformed.
    ($txt:expr) => {
        log::debug!("[{}:{}] {}:", $crate::file_name!(), ::core::line!(), $txt)
    };
    ($txt:expr, $val:expr $(,)?) => {
        // Use of `match` here is intentional because it affects the lifetimes
        // of temporaries - https://stackoverflow.com/a/48732525/1063961
        match $val {
            tmp => {
                log::debug!("[{}:{}] {}: {} = {:#?}",
                    $crate::file_name!(), ::core::line!(), $txt, ::core::stringify!($val), &tmp);
                tmp
            }
        }
    };
    ($txt:expr, ($val:expr),+ $(,)?) => {
        $crate::dbg!($txt);
        ($($crate::dbg!($val)),+,)
    };
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)+) => (log::trace!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}
#[macro_export]
macro_rules! debug {
    ($($arg:tt)+) => (log::debug!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}
#[macro_export]
macro_rules! warn {
    ($($arg:tt)+) => (log::warn!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}
#[macro_export]
macro_rules! error {
    ($($arg:tt)+) => (log::error!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}
#[macro_export]
macro_rules! info {
    ($($arg:tt)+) => (log::info!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}
#[macro_export]
macro_rules! fatal {
    ($($arg:tt)+) => (log::fatal!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), core::format_args!($($arg)+)))
}

// TODO: can optimize to a single pattern, can maybe also hook into the log infrastructure to do this more elegant, including hiding the term param
#[macro_export]
macro_rules! term_info {
    ($format:expr, $($arg:tt)+) => {
        let __term_txt = alloc:: format!($format, $($arg)+);
        $crate::terminal::term().add_text_new_line(&__term_txt);
        log::info!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &__term_txt)
    };
    ($__term_txt:expr) => {
        $crate::terminal::term().add_text_new_line(&$__term_txt);
        log::info!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &$__term_txt)
    }
}
#[macro_export]
macro_rules! term_info_same_line {
    ($format:expr, $($arg:tt)+) => {
        let __term_txt = alloc:: format!($format, $($arg)+);
        $crate::terminal::term().add_text_same_line(&__term_txt);
        log::info!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &__term_txt)
    };
    ($__term_txt:expr) => {
        $crate::terminal::term().add_text_same_line(&$__term_txt);
        log::info!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &$__term_txt)
    }
}

#[macro_export]
macro_rules! term_error {
    ($format:expr, $($arg:tt)+) => {
        let __term_txt = alloc:: format!($format, $($arg)+);
        $crate::terminal::term().add_text_new_line(&__term_txt);
        log::error!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &__term_txt)
    };
    ($__term_txt:expr) => {
        $crate::terminal::term().add_text_new_line(&$__term_txt);
        log::error!("[{}:{}] {}", $crate::file_name!(), ::core::line!(), &$__term_txt)
    }
}
