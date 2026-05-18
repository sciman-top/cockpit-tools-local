use std::process::ExitCode;

fn print_usage() {
    println!(
        "Usage: codex-local-access-gateway --serve [--restore-disabled-on-exit]\n\
         Starts the Cockpit local access gateway in this process for smoke tests."
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    if !args.iter().any(|arg| arg == "--serve") {
        print_usage();
        return ExitCode::from(2);
    }

    let restore_disabled_on_exit = args.iter().any(|arg| arg == "--restore-disabled-on-exit");

    match antigravity_cockpit_tools_lib::local_hardened_api_smoke::enable_gateway().await {
        Ok(state_json) => {
            println!("HLA_GATEWAY_READY {}", state_json);
        }
        Err(err) => {
            eprintln!("HLA_GATEWAY_ERROR {}", err);
            return ExitCode::FAILURE;
        }
    }

    if let Err(err) = tokio::signal::ctrl_c().await {
        eprintln!("HLA_GATEWAY_SIGNAL_ERROR {}", err);
        return ExitCode::FAILURE;
    }

    if restore_disabled_on_exit {
        if let Err(err) =
            antigravity_cockpit_tools_lib::local_hardened_api_smoke::disable_gateway().await
        {
            eprintln!("HLA_GATEWAY_RESTORE_ERROR {}", err);
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}
