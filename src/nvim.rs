extern crate rmp;
extern crate rmp_serde;
extern crate serde;

use std::io::{stdout, Write, Error};
use std::cell::RefCell;
use std::process::{Command, Child, Stdio, ChildStdout, ChildStdin};
use std::collections::BTreeMap;

use self::rmp_serde::{Serializer, Deserializer};
use self::serde::{Serialize, Deserialize};

use highlight;

const HEIGHT : usize = 100;
const WIDTH : usize = 100;

const FIRST_DRAW: usize = 1;
const MODELINE: usize = 2;
const EOF: usize = 4;
const FINISHED: usize = 8;

pub struct Nvim<'a> {
    deserializer:   Deserializer<ChildStdout>,
    serializer:     RefCell<Serializer<'a, rmp_serde::encode::StructArrayWriter> >,
    cursor:         [usize; 2],
    expected_line:  usize,
    offset:         usize,
    pub state:      usize,
    hl:             highlight::Highlight,
    default_hl:     highlight::Highlight,
}

impl<'a> Nvim<'a> {
    pub fn start_process(file: &str) -> Child {
        Command::new("nvim")
            .arg("--embed")
            .arg("-nZ")
            .arg("+0")
            .arg("-c").arg("set scrolloff=0 mouse= showtabline=0 | doautocmd BufRead Cargo.toml | NoMatchParen")
            // .arg("--")
            // .arg(file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("could not find nvim")
    }

    pub fn new(stdin: &'a mut ChildStdin, stdout: ChildStdout) -> Self {
        let serializer = Serializer::new(stdin);
        let deserializer = Deserializer::new(stdout);

        Nvim {
            deserializer: deserializer,
            serializer: RefCell::new(serializer),
            cursor: [0, 0],
            expected_line: 0,
            state: FIRST_DRAW,
            offset: 0,
            hl: highlight::Highlight::new(),
            default_hl: highlight::Highlight::new(),
        }
    }

    pub fn set_eof(&mut self) {
        self.state |= EOF;
    }

    pub fn nvim_command(&self, command: &str) -> Result<(), self::rmp_serde::encode::Error> {
        let value = ( 0, 300, "nvim_command", (command,) );
        value.serialize(&mut *self.serializer.borrow_mut())
    }

    pub fn attach(&self) -> Result<(), self::rmp_serde::encode::Error> {
        let mut kwargs = BTreeMap::new();
        kwargs.insert("rgb", true);
        let value = ( 0, 100, "nvim_ui_attach", (WIDTH, HEIGHT, kwargs) );
        value.serialize(&mut *self.serializer.borrow_mut())
    }

    pub fn quit(&self) -> Result<(), self::rmp_serde::encode::Error> {
        self.nvim_command("qa!")
    }

    fn scroll(&self, down: usize) -> Result<(), self::rmp_serde::encode::Error> {
        self.nvim_command(format!("normal {}gjz\n", down).as_str())
    }

    pub fn add_lines(&mut self, lines: &[&str]) -> Result<(), self::rmp_serde::encode::Error> {
        // println!("{:?}", lines);
        let value = ( 0, 100, "nvim_buf_set_lines", (1, -1, -1, false, lines) );
        self.expected_line += lines.len();
        value.serialize(&mut *self.serializer.borrow_mut())
    }

    pub fn print_newline(&self) -> Result<(), Error> {
        stdout().write(self.default_hl.to_string().as_bytes())?;
        stdout().write(b"\x1b[K\n")?;
        Ok(())
    }

    fn handle_put(&mut self, args: &[rmp::Value]) -> Result<(), Error> {
        if self.state & MODELINE != 0 {
            // hit modeline, finished first draw
            self.state &= ! FIRST_DRAW;
        }

        if self.state & (FIRST_DRAW | MODELINE | FINISHED) != 0 || self.expected_line < self.cursor[0] {
            return Ok(());
        }

        let eofstr = format!("~{1:0$}", WIDTH - 1, "");

        let parts : Vec<_> = args
            .iter()
            .flat_map(|x| x.as_array().unwrap())
            .map(|x| x.as_str().unwrap())
            .collect()
            ;
        let string = parts.join("");
        // println!("{} {:?} {}", self.expected_line, string, self.offset);

        if string != eofstr || true {
            if self.offset != 0 {
                stdout().write(self.default_hl.to_string().as_bytes())?;
                write!(&mut stdout(), "{1:0$}", self.offset, "")?;
            }
            stdout().write(self.hl.to_string().as_bytes())?;
            stdout().write(string.as_bytes())?;
            stdout().flush()?;

            self.cursor[1] += self.offset + string.len();
            self.offset = 0;
        }

        Ok(())
    }

    fn handle_cursor_goto(&mut self, args: &[rmp::Value]) -> Result<(), Error> {
        let pos = match args.last() {
            Some(a) => a.as_array().unwrap(),
            None => return Ok(())
        };

        let row = pos[0].as_u64().unwrap() as usize;
        let col = pos[1].as_u64().unwrap() as usize;
        self.state &= ! MODELINE;
        self.offset = col;

        // println!("{:?}--{:?}#{}", (row, col), self.cursor, self.expected_line);
        if self.state & (FIRST_DRAW | EOF) == EOF && self.expected_line == self.cursor[0] && row != self.cursor[0] {
            self.quit().unwrap();
            self.state |= FINISHED;
            return self.print_newline()
        }

        if row >= HEIGHT - 2 {
            // end of page, jumped to modelines
            self.state |= MODELINE;
            if (self.state & FIRST_DRAW) == 0 {
                self.scroll(HEIGHT - 2).unwrap();

            }
            self.cursor = [0, 0];
            self.offset = 0;
            return Ok(());

        } else if (self.state & FIRST_DRAW) != 0 || self.expected_line < row {
            return Ok(())

        } else if row == self.cursor[0]+1 {
            // new line
            self.cursor = [row, 0];

        } else if row == self.cursor[0] && col > self.cursor[1] {
            // moved right on same line
            self.offset -= self.cursor[1];
            self.cursor[0] = row;
            return Ok(())

        } else {
            return Ok(())
        }

        // println!("{:?}--{:?}", (row, col), self.cursor);
        if self.state & FIRST_DRAW == 0 {
            self.print_newline()?
        }
        Ok(())
    }

    fn handle_highlight_set(&mut self, args: &[rmp::Value]) {
        let hl = match args.last().and_then(|x| x.as_array().unwrap().last()) {
            Some(a) => a.as_map().unwrap(),
            None => {
                self.hl = self.default_hl.clone();
                return
            },
        };

        let mut fg : Option<String> = None;
        let mut bg : Option<String> = None;
        let mut attrs = self.default_hl.attrs;

        for &(ref key, ref value) in hl.iter() {
            let mut bit : Option<highlight::Attr> = None;

            match key.as_str().unwrap() {
                "foreground" => {
                    fg = Some( highlight::rgb_to_string(value.as_u64().unwrap() as u32) );
                },
                "background" => {
                    bg = Some( highlight::rgb_to_string(value.as_u64().unwrap() as u32) );
                },
                "reverse" => {
                    bit = Some(highlight::Attr::REVERSE);
                }
                "bold" => {
                    bit = Some(highlight::Attr::BOLD);
                },
                "italic" => {
                    bit = Some(highlight::Attr::ITALIC);
                },
                "underline" => {
                    bit = Some(highlight::Attr::UNDERLINE);
                },
                _ => (),
            }

            match bit {
                Some(bit) => {
                    if value.as_bool().unwrap() {
                        attrs |= bit as u8;
                    } else {
                        attrs &= !( bit as u8 );
                    }
                },
                None => (),
            }
        }

        self.hl.fg = fg.unwrap_or_else(|| self.default_hl.fg.clone());
        self.hl.bg = bg.unwrap_or_else(|| self.default_hl.bg.clone());
        self.hl.attrs = attrs;
    }

    fn handle_update(&mut self, update: &rmp::Value) -> Result<(), Error> {
        let update = update.as_array().unwrap();
        // println!("\n{:?}", update);
        match update[0].as_str().unwrap() {
            "put" => {
                self.handle_put(&update[1..])?;
            },
            "cursor_goto" => {
                self.handle_cursor_goto(&update[1..])?;
            },
            "highlight_set" => {
                self.handle_highlight_set(&update[1..]);
            },
            "update_fg" => {
                match update[1..].last().and_then(|x| x.as_array().unwrap().last()) {
                    Some(x) => {
                        self.default_hl.fg = highlight::rgb_to_string(x.as_u64().unwrap() as u32);
                    },
                    None => ()
                };
            },
            "update_bg" => {
                match update[1..].last().and_then(|x| x.as_array().unwrap().last()) {
                    Some(x) => {
                        self.default_hl.bg = highlight::rgb_to_string(x.as_u64().unwrap() as u32);
                    },
                    None => ()
                };
            },
            _ => (),
        }

        Ok(())
    }

    pub fn process_event(&mut self) -> Result<bool, Error> {
        if self.state & FINISHED != 0 {
            return Ok(false)
        }

        let value : rmp_serde::Value = Deserialize::deserialize(&mut self.deserializer).unwrap();
        let value = value.as_array().unwrap();
        // println!("\n{:?}", value);
        match value[0].as_u64().unwrap() {
            2 => {
                // notification
                let method = value[1].as_str().unwrap();
                if method == "redraw" {
                    let params = value[2].as_array().unwrap();
                    for update in params {
                        self.handle_update(update)?;
                        if self.state & FINISHED != 0 {
                            return Ok(true)
                        }
                    }
                }
            },
            1 => {
                // response
                // println!("{:?}", value[2]);
            },
            _ => (),
        }

        Ok(false)
    }
}