cargo build --release
rm -rf ~/.local/bin/drop-ins
ln target/release/drop-ins ~/.local/bin/drop-ins
