//! Publishes an already decoded adaptation bundle. This does not decode native
//! client resources; the input is the seven-family JSON export. The constants
//! file is the matching client's `shipAdaptionSystem/common/const.py`.
//! Runtime SDE reference closure and
//! node, task and effect gameplay semantics are not validated here.

mod adaption_content;

fn main() {
    if let Err(error) = adaption_content::run(std::env::args().skip(1)) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
