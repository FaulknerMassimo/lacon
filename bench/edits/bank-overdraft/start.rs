use std::collections::BTreeMap;
use std::io::{self, Read};

/// Arguments each command takes after its name: (fewest, most).
const ARITY: &[(&str, usize, usize)] = &[
    ("open", 1, 2),
    ("deposit", 2, 2),
    ("withdraw", 2, 2),
    ("transfer", 3, 3),
    ("close", 1, 1),
    ("interest", 1, 1),
    ("history", 1, 1),
    ("balance", 1, 1),
];

struct Entry {
    line: usize,
    what: String,
    amount: i64,
}

struct Account {
    balance: i64, // in cents
    history: Vec<Entry>,
    closed: bool,
}

impl Account {
    fn new(balance: i64) -> Self {
        Account { balance, history: Vec::new(), closed: false }
    }
}

type Accounts = BTreeMap<String, Account>;

/// Digits, optionally followed by a point and one or two digits, in cents.
fn parse_cents(s: &str) -> Option<i64> {
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (s, None),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cents = match frac {
        None => 0,
        Some(f) => {
            if f.is_empty() || f.len() > 2 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            format!("{:0<2}", f).parse::<i64>().ok()?
        }
    };
    Some(whole.parse::<i64>().ok()? * 100 + cents)
}

/// An amount in cents that must not be zero.
fn positive(s: &str) -> Option<i64> {
    parse_cents(s).filter(|&c| c != 0)
}

fn fmt_cents(c: i64) -> String {
    format!("{}.{:02}", c / 100, c % 100)
}

fn record(acct: &mut Account, n: usize, what: &str, amount: i64) {
    acct.history.push(Entry { line: n, what: what.to_string(), amount });
}

/// Why the first of names is not an open account, or None.
fn check_open(accounts: &Accounts, names: &[&str]) -> Option<String> {
    for name in names {
        match accounts.get(*name) {
            None => return Some(format!("no account {}", name)),
            Some(a) if a.closed => return Some(format!("account closed {}", name)),
            _ => {}
        }
    }
    None
}

fn cmd_open(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    let name = args[0];
    let amount = if args.len() == 2 { parse_cents(args[1]) } else { Some(0) };
    let Some(amount) = amount else {
        return Some("bad amount".to_string());
    };
    if accounts.contains_key(name) {
        return Some("account exists".to_string());
    }
    let mut acct = Account::new(amount);
    record(&mut acct, n, "open", amount);
    accounts.insert(name.to_string(), acct);
    None
}

fn cmd_deposit(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    let Some(amount) = positive(args[1]) else {
        return Some("bad amount".to_string());
    };
    if let Some(why) = check_open(accounts, &args[..1]) {
        return Some(why);
    }
    let acct = accounts.get_mut(args[0]).unwrap();
    acct.balance += amount;
    record(acct, n, "deposit", amount);
    None
}

fn cmd_withdraw(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    let Some(amount) = positive(args[1]) else {
        return Some("bad amount".to_string());
    };
    if let Some(why) = check_open(accounts, &args[..1]) {
        return Some(why);
    }
    let acct = accounts.get_mut(args[0]).unwrap();
    if acct.balance < amount {
        return Some("insufficient funds".to_string());
    }
    acct.balance -= amount;
    record(acct, n, "withdraw", amount);
    None
}

fn cmd_transfer(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    let (src, dst) = (args[0], args[1]);
    let Some(amount) = positive(args[2]) else {
        return Some("bad amount".to_string());
    };
    if let Some(why) = check_open(accounts, &[src, dst]) {
        return Some(why);
    }
    if src == dst {
        return Some("same account".to_string());
    }
    if accounts[src].balance < amount {
        return Some("insufficient funds".to_string());
    }
    let from = accounts.get_mut(src).unwrap();
    from.balance -= amount;
    record(from, n, &format!("transfer to {}", dst), amount);
    let to = accounts.get_mut(dst).unwrap();
    to.balance += amount;
    record(to, n, &format!("transfer from {}", src), amount);
    None
}

fn cmd_close(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    if let Some(why) = check_open(accounts, args) {
        return Some(why);
    }
    let acct = accounts.get_mut(args[0]).unwrap();
    if acct.balance != 0 {
        return Some("balance not zero".to_string());
    }
    acct.closed = true;
    record(acct, n, "close", 0);
    None
}

/// Adds RATE percent to every open account with a positive balance,
/// rounded down to the cent.
fn cmd_interest(accounts: &mut Accounts, n: usize, args: &[&str]) -> Option<String> {
    let Some(rate) = positive(args[0]) else {
        return Some("bad amount".to_string());
    };
    for acct in accounts.values_mut() {
        if acct.closed || acct.balance <= 0 {
            continue;
        }
        let gain = acct.balance * rate / 10000;
        if gain > 0 {
            acct.balance += gain;
            record(acct, n, "interest", gain);
        }
    }
    None
}

fn cmd_history(accounts: &Accounts, args: &[&str]) -> Option<String> {
    let name = args[0];
    let Some(acct) = accounts.get(name) else {
        return Some(format!("no account {}", name));
    };
    println!("{}:", name);
    for e in &acct.history {
        println!("  {} {} {}", e.line, e.what, fmt_cents(e.amount));
    }
    None
}

fn cmd_balance(accounts: &Accounts, args: &[&str]) -> Option<String> {
    if let Some(why) = check_open(accounts, args) {
        return Some(why);
    }
    println!("{} {}", args[0], fmt_cents(accounts[args[0]].balance));
    None
}

/// Carries out one command; returns why it could not, or None.
fn run_line(accounts: &mut Accounts, n: usize, line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let Some((&cmd, args)) = fields.split_first() else {
        return None;
    };
    let Some(&(_, lo, hi)) = ARITY.iter().find(|(name, _, _)| *name == cmd) else {
        return Some("bad command".to_string());
    };
    if args.len() < lo || args.len() > hi {
        return Some("bad command".to_string());
    }
    match cmd {
        "open" => cmd_open(accounts, n, args),
        "deposit" => cmd_deposit(accounts, n, args),
        "withdraw" => cmd_withdraw(accounts, n, args),
        "transfer" => cmd_transfer(accounts, n, args),
        "close" => cmd_close(accounts, n, args),
        "interest" => cmd_interest(accounts, n, args),
        "history" => cmd_history(accounts, args),
        _ => cmd_balance(accounts, args),
    }
}

fn print_summary(accounts: &Accounts) {
    let mut total = 0;
    for (name, acct) in accounts {
        if acct.closed {
            continue;
        }
        println!("{} {}", name, fmt_cents(acct.balance));
        total += acct.balance;
    }
    println!("total {}", fmt_cents(total));
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut accounts = Accounts::new();
    for (i, line) in input.lines().enumerate() {
        let n = i + 1;
        if let Some(why) = run_line(&mut accounts, n, line) {
            println!("line {}: {}", n, why);
        }
    }
    print_summary(&accounts);
}
