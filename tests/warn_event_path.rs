//! `WarnEvent::path`: the stylesheet a diagnostic came from, as an identity —
//! for a file the importer loaded, the importer's canonical URL (the resolved
//! absolute path with `FsImporter`); for the entry stylesheet, `Options::url`
//! exactly as supplied. That is what a CLI needs to tell a load-path
//! dependency from the entry (dart-sass `--quiet-deps`), since
//! `WarnEvent::url` is dart's short display form (a load-path file's basename).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use sasso::{compile, FsImporter, Options, WarnEvent};

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sasso_warn_path_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lp")).expect("mkdir");
    dir
}

#[test]
fn warn_event_path_is_the_resolved_file_path() {
    let dir = scratch();
    let entry = dir.join("entry.scss");
    std::fs::write(&entry, "@import \"dep\";\n@warn \"from entry\";\n").unwrap();
    std::fs::write(
        dir.join("lp/_dep.scss"),
        "@import \"dep2\";\n@warn \"from dep\";\n",
    )
    .unwrap();
    std::fs::write(dir.join("lp/_dep2.scss"), "e { f: 2; }\n").unwrap();

    // (message, deprecation?, url, path) per event.
    type Seen = Vec<(String, bool, String, String)>;
    let seen: Rc<RefCell<Seen>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let importer = FsImporter::new(vec![dir.join("lp")]);
    let url = entry.to_string_lossy().into_owned();
    let opts = Options::default()
        .with_importer(&importer)
        .with_url(&url)
        .with_warn_handler(Rc::new(move |ev: &WarnEvent<'_>| {
            sink.borrow_mut().push((
                ev.message.to_string(),
                ev.deprecation,
                ev.url.to_string(),
                ev.path.to_string(),
            ));
        }));
    let src = std::fs::read_to_string(&entry).unwrap();
    compile(&src, &opts).expect("compile");
    let events = seen.borrow();

    let dep_canon = std::fs::canonicalize(dir.join("lp/_dep.scss")).unwrap();
    let dep_canon = dep_canon.to_string_lossy();
    // The entry's own @import deprecation: path is the entry (as given).
    let entry_dep = events
        .iter()
        .find(|(m, d, _, _)| *d && m.contains("@import"))
        .expect("entry deprecation");
    assert_eq!(entry_dep.3, url, "entry deprecation carries the entry path");
    // The dependency's @import deprecation: display url is dart's short form,
    // path is the resolved file.
    let dep_dep = events
        .iter()
        .filter(|(m, d, _, _)| *d && m.contains("@import"))
        .nth(1)
        .expect("dependency deprecation");
    assert_eq!(dep_dep.2, "_dep.scss", "display url stays dart's short form");
    assert_eq!(dep_dep.3, dep_canon, "path is the resolved load-path file");
    // @warn from the dependency and from the entry.
    let w_dep = events
        .iter()
        .find(|(m, ..)| m == "from dep")
        .expect("@warn from dep");
    assert_eq!(w_dep.3, dep_canon);
    let w_entry = events
        .iter()
        .find(|(m, ..)| m == "from entry")
        .expect("@warn from entry");
    assert_eq!(w_entry.3, url);
    drop(events);
    std::fs::remove_dir_all(&dir).ok();
}
