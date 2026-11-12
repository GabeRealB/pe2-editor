//! `pe2-editor`: see the library, which is everything but this.

fn main() -> editor::Result {
    let done = editor::run();
    // Ending in order: what it shares with the project's other processes is left as the next may trust it.
    project::shared::rest_all();
    done
}
