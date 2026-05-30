#[cfg(feature = "log")]
pub(crate) use ::log::{error, warn};

#[cfg(not(feature = "log"))]
macro_rules! error {
    ($($t:tt)*) => { eprint!("Error: "); eprintln!($($t)*) };
}
#[cfg(not(feature = "log"))]
pub(crate) use error;

#[cfg(not(feature = "log"))]
macro_rules! _warn {
    ($($t:tt)*) => { eprint!("Warning: "); eprintln!($($t)*) };
}
#[cfg(not(feature = "log"))]
pub(crate) use _warn as warn;
