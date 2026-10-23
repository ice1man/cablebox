//! Read-only check of the D-Bus integrations: `cargo run -p sysctl --example probe`.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match sysctl::network::status().await {
        Ok(s) => println!("network: {s:?}"),
        Err(e) => println!("network: error: {e}"),
    }
    match sysctl::updates::check(false).await {
        Ok(u) => {
            println!("updates: {} pending", u.len());
            for p in u.iter().take(10) {
                println!("  {} {}{}", p.name, p.version, if p.important { " (important)" } else { "" });
            }
        }
        Err(e) => println!("updates: error: {e}"),
    }
    println!("reboot required: {}", sysctl::power::reboot_required());
    for unit in [sysctl::services::SSH, sysctl::services::CONSOLE, "cablebox.service"] {
        match sysctl::services::state(unit).await {
            Ok(s) => println!("{unit}: {s:?}"),
            Err(e) => println!("{unit}: error: {e}"),
        }
    }
    for unit in ["dbus.service", "cablebox.service"] {
        println!("{unit} MainPID: {:?}", sysctl::services::main_pid(unit).await.map_err(|e| e.to_string()));
    }
    println!("this process is cablebox.service: {}", sysctl::services::is_main_process_of("cablebox.service").await);
}
