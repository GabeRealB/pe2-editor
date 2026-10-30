//! Events: what makes a graph do something. An event is only that it happened: what happened is an output's to say, as a gate has no one payload to
//! give. In Rust an event is a marker carrying its name; declarations and the contract write the name.

/// An event, as extensions name it.
pub trait Event {
    const NAME: &'static str;
    const LABEL: &'static str;
}

/// Declare the events every project has.
macro_rules! events {
    ($($(#[$doc:meta])* $marker:ident = $name:literal, $label:literal;)*) => {
        $(
            $(#[$doc])*
            pub struct $marker;

            impl Event for $marker {
                const NAME: &'static str = $name;
                const LABEL: &'static str = $label;
            }
        )*
        /// Every built-in event, by name, with its label.
        pub const BUILT_IN: &[(&str, &str)] = &[$(($name, $label)),*];
    };
}

events! {
    /// A build was asked for: what answers it is pulled.
    Build = "pe2.events@build", "Build";
    /// A clean was asked for: what answers it is invalidated, and what it deployed goes.
    Clean = "pe2.events@clean", "Clean";
    /// Files of the project changed on disk, by the editor or another program.
    FilesChanged = "pe2.events@files-changed", "Files changed";
    /// Another config was chosen.
    ConfigChanged = "pe2.events@config-changed", "Config changed";
    /// The project was opened.
    ProjectOpened = "pe2.events@project-opened", "Project opened";
}
