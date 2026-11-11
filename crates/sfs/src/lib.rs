//! The filesystem a project's processes share. Every operation on a project's files - what programs do through the
//! folder it is served at, what whoever holds the project writes to its layers - goes through this library, in
//! whichever process: the server, a host, the editor, the command line. Its state is shared between them by
//! mappings of files in the project's cache:
//!
//! - `log`: a write-ahead log that holds every operation in transit, with its bytes. Any process appends; all read
//!   it where it is mapped. A position in it is the sequence number of everything.
//! - `content`: a file in transit, as the ranges of the log its bytes are in.
//! - `header`: one page of positions every process reads without asking another - how far the log goes, how far
//!   the project has it, how far that is on disk.
//!
//! It knows nothing of what a project is: it is given a folder to keep its state in.

pub mod content;
pub mod header;
pub mod log;

pub use content::Content;
pub use header::Header;
pub use log::{Log, Record};
