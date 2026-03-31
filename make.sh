cargo build --release
rm -rf ~/.local/bin/drop-ins
rm -rf ~/.local/bin/overlay-music
ln target/release/overlay-music ~/.local/bin/overlay-music
