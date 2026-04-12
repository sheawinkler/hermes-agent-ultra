pub fn normalize_gateway_action(action: Option<&str>) -> &'static str {
    match action.unwrap_or("status") {
        "start" => "start",
        "stop" => "stop",
        "restart" => "restart",
        _ => "status",
    }
}
