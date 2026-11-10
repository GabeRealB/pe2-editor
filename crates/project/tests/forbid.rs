//! A process that only serves a project's files loads no extension, whatever it is given: in a process of its own,
//! since what it forbids it forbids for good.

#[test]
fn a_process_that_forbids_extensions_loads_none() {
    let origin = || project::ext::Origin::Component(std::path::PathBuf::from("an.extension"));
    // Not forbidden, what is no component fails as that.
    let refused = project::ext::component::load_bytes("an.extension", b"not a component", origin()).err().unwrap();
    assert!(!format!("{refused:#}").contains("loads no extension"), "{refused:#}");
    project::ext::forbid();
    assert!(project::ext::forbidden());
    let refused = project::ext::component::load_bytes("an.extension", b"not a component", origin()).err().unwrap();
    assert!(format!("{refused:#}").contains("an.extension: this process loads no extension"), "{refused:#}");
}
