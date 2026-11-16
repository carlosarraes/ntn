mod cli;
mod client;
mod commands;
mod config;
mod curl;
mod db;
mod error;
mod ids;
mod output;
mod page;
mod records;
mod time;

fn main() {
    std::process::exit(cli::run());
}
