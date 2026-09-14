//! Plain-CSS files reached through `@use`/`@import` are emitted the way
//! dart-sass emits them: nothing of the file's own `@charset` survives (the
//! output's `@charset` is re-derived from its content), and nested rules keep
//! the selector list's source line structure.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use sasso::{compile, FsImporter, Options, WarnEvent};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sasso_plaincss_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Compile `src` as an entry in `dir`, swallowing warnings (the `@import`
/// deprecation), and return the CSS.
fn compile_in(dir: &std::path::Path, name: &str, src: &str) -> String {
    let entry = dir.join(name);
    std::fs::write(&entry, src).unwrap();
    let url = entry.to_string_lossy().into_owned();
    let imp = FsImporter::new(Vec::new());
    let sink: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&sink);
    let opts = Options::default()
        .with_importer(&imp)
        .with_url(&url)
        .with_warn_handler(Rc::new(move |ev: &WarnEvent<'_>| {
            seen.borrow_mut().push(ev.message.to_string());
        }));
    compile(src, &opts).expect("compile")
}

#[test]
fn a_loaded_files_charset_is_dropped() {
    // dart: the `@charset` of a loaded `.css` (or `.scss`) file never appears
    // in the output; the output's own `@charset "UTF-8";` comes from its
    // non-ASCII content, and an all-ASCII output has none.
    let dir = scratch("charset");
    std::fs::write(
        dir.join("_theme.css"),
        "@charset \"utf-8\";\n.t { color: red; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("_lib.scss"),
        "@charset \"utf-8\";\n.s { content: \"é\"; }\n",
    )
    .unwrap();
    assert_eq!(
        compile_in(&dir, "use.scss", "@use \"theme\";\n@use \"lib\";\na { b: c }\n"),
        "@charset \"UTF-8\";\n.t {\n  color: red;\n}\n\n.s {\n  content: \"é\";\n}\n\na {\n  b: c;\n}"
    );
    assert_eq!(
        compile_in(
            &dir,
            "imp.scss",
            "a { b: c }\n@import \"theme\";\n@import \"lib\";\n"
        ),
        "@charset \"UTF-8\";\na {\n  b: c;\n}\n\n.t {\n  color: red;\n}\n\n.s {\n  content: \"é\";\n}"
    );
    assert_eq!(
        compile_in(&dir, "only.scss", "@import \"theme\";\n"),
        ".t {\n  color: red;\n}"
    );
    // Only the file's top-level `@charset` goes: one inside an at-rule or a
    // style rule is kept verbatim, as dart keeps it.
    std::fs::write(
        dir.join("_nested.css"),
        "@media (min-width: 1px) {\n  @charset \"utf-8\";\n  .t { color: red; }\n}\n.u { @charset \"utf-8\"; color: blue; }\n",
    )
    .unwrap();
    assert_eq!(
        compile_in(&dir, "usenested.scss", "@use \"nested\";\n"),
        "@media (min-width: 1px) {\n  @charset \"utf-8\";\n  .t {\n    color: red;\n  }\n}\n.u {\n  @charset \"utf-8\";\n  color: blue;\n}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
