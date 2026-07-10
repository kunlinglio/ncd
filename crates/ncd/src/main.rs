use ncd::app;

#[tokio::main]
async fn main() {
    match app::parse() {
        Some(args) => match args.command {
            app::Command::Run => app::cmd_run(args.verbose).await,
            app::Command::List => app::cmd_list(),
        },
        None => {}
    }
}
