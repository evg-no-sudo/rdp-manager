fn main() {
    println!("cargo:rerun-if-changed=native/rdp_bridge.c");
    println!("cargo:rerun-if-changed=native/rdp_bridge.h");
    let library = pkg_config::Config::new()
        .atleast_version("3.25")
        .probe("freerdp3")
        .expect("FreeRDP 3.25+ development headers are required (freerdp3-dev)");
    let winpr = pkg_config::Config::new()
        .atleast_version("3.0")
        .probe("winpr3")
        .expect("WinPR 3 development headers are required (libwinpr3-dev)");
    let mut build = cc::Build::new();
    build
        .file("native/rdp_bridge.c")
        .std("gnu11")
        .warnings(true);
    for path in library
        .include_paths
        .iter()
        .chain(winpr.include_paths.iter())
    {
        build.flag("-isystem").flag(path.as_os_str());
    }
    build.compile("rdp_manager_bridge");
}
