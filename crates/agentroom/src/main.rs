fn main() {
    eprintln!(
        "\n\x1b[33m⚠  `agentroom` is deprecated.\x1b[0m\n\n\
         The project has been renamed to \x1b[1mroom-cli\x1b[0m.\n\n\
         To migrate:\n\n\
         \x20 cargo uninstall agentroom\n\
         \x20 cargo install room-cli\n\n\
         The binary is now called `room` (unchanged).\n\
         See https://github.com/knoxio/room for details.\n"
    );
}
