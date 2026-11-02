// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `dblens --mcp`: the MCP server on stdio, headless (src/mcp.rs).
    if dblens_lib::mcp::requested(std::env::args()) {
        std::process::exit(dblens_lib::mcp::run_stdio());
    }
    dblens_lib::run()
}
