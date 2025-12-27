# Midi Reader and Editor

## Description
A midi reader, player and editor made in Rust. Can be compiled on multiple build targets including web.

## Demo
A demo is currently available on [My Website](https://ethanfirst.com/midi.html)

## Installation
The program must be built from source. Cross-Compilation from source is not currently an option because MIDIR requires platform specific system libraries. 

### To build from source:
1. ensure rust is properly installed [Instructions](https://rust-lang.org)
2. enter directory ~\hw4-template\
3. run `cargo build --release`
4. the executable for the platform of choice should be found in ~\hw4-template\target\release

### For web execution:
1. ensure rust is properly installed [Instructions](https://rust-lang.org)
2. enter directory ~\hw4-template\
3. build the web source by running `trunk build` or use `trunk build --release` to build a folder that can be uploaded to a hosting service
4. if the first command was used, run `trunk serve` from the hw4-template folder. This should open a local server on `http://127.0.0.1:8080/index.html#dev`

## Usage
On web, drag and drop a file to listen or modify it. Locally, you can use the GUI buttons on the main menu.
Notes can be clicked on to be added or removed. Once you are done you can press exit and the save button will save an exported midi to the same location as where an uploaded file.
