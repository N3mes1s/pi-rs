use ignore::WalkBuilder;
use std::path::Path;
fn main() {
    let root = Path::new(".");
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .hidden(false)
        .build();
    let mut found = 0usize;
    for ent in walker {
        let ent = match ent { Ok(e) => e, Err(_) => continue };
        let p = ent.path();
        let s = p.to_string_lossy();
        if s == "./.git" || s.starts_with("./.git/") {
            println!("{}", s);
            found += 1;
            if found >= 20 { break; }
        }
    }
    if found == 0 { println!("NO_GIT"); }
}
