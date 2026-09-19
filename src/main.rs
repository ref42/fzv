//! The `fzv` binary: everything lives in the library, this only sets the exit
//! code.

fn main() {
    std::process::exit(fzv::entry());
}
