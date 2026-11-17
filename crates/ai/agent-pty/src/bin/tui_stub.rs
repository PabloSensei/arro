//! Stands in for a harness in the tests of this crate: prints a saved screen, then says what it
//! receives. `tui-stub <screen file>`

use std::io::{self, BufRead};

fn main() {
    let path = std::env::args().nth(1).expect("a screen file");
    print!("{}", std::fs::read_to_string(path).expect("the screen"));
    for line in io::stdin().lock().lines() {
        println!("GOT {}", line.expect("stdin"));
    }
}
