//! `/antigravity` (`/ag`) chat command help text.

pub(super) fn print_chat_ag_help() {
    eprintln!("Antigravity commands:");
    eprintln!("  /ag [status]               Show multi-account pool status & cooldowns");
    eprintln!("  /ag quota [cached]         Show remaining Claude/Gemini quota (live or cached)");
    eprintln!("  /ag use <id|email|index>   Switch active sticky Antigravity account");
    eprintln!("  /ag enable <id|email|idx>  Enable a disabled or flagged account");
    eprintln!("  /ag disable <id|email|idx> Disable an account from rotation");
    eprintln!("  /ag models [refresh]       List or refresh `google/antigravity-*` models");
    eprintln!("Tip: use `opencode auth login` to add Google accounts via OAuth.");
}
