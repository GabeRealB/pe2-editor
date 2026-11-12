//! What a process holding a project does besides its own work, whether it has a window (the editor) or not (a host
//! started for what commands ask): it feeds what serves the project's folder (`files`), runs the epochs events set
//! off (`runner`), and watches the project's layers for what changes by other means than itself (`watch`). Nothing
//! here knows a window: what is to be woken when something ends is given as a call.

pub mod files;
pub mod runner;
pub mod says;
pub mod watch;
