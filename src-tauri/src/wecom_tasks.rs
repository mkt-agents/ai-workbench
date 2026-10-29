//! Shared-task metadata only. Call from spawn_blocking while holding DbState.conn;
//! no process-global cache, message bodies, credentials or async runtime dependencies.
use rand::TryRngCore;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub const INACTIVITY_MS: i64 = 24 * 60 * 60 * 1000;
pub const MAX_ACTIVE_TASKS: usize = 200;
pub const MAX_MEMBERS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskRoute {
    pub reply: Option<String>,
    pub task_id: Option<String>,
    pub owner_id: Option<String>,
    pub ended_task_id: Option<String>,
}

pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS shared_tasks (
            task_id TEXT PRIMARY KEY NOT NULL,
            bot_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            owner_id TEXT NOT NULL,
            name TEXT NOT NULL,
            members_json TEXT NOT NULL DEFAULT '[]',
            created_at_ms INTEGER NOT NULL,
            touched_at_ms INTEGER NOT NULL,
            ended_at_ms INTEGER
        );
        CREATE INDEX IF NOT EXISTS shared_tasks_scope
          ON shared_tasks(bot_id, group_id, ended_at_ms);
        CREATE INDEX IF NOT EXISTS shared_tasks_expiry
          ON shared_tasks(ended_at_ms, touched_at_ms);",
    )
    .map_err(|e| e.to_string())
}

#[derive(Debug)]
struct Task {
    id: String,
    owner: String,
    name: String,
    members: Vec<String>,
}

fn begin(conn: &Connection) -> Result<Transaction<'_>, String> {
    // IMMEDIATE serializes check-and-write across connections too (not only Arc<Mutex>).
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|e| e.to_string())
}

fn expire(conn: &Connection, now_ms: i64) -> Result<(), String> {
    conn.execute(
        "UPDATE shared_tasks SET ended_at_ms=?1, members_json='[]'
         WHERE ended_at_ms IS NULL AND touched_at_ms <= ?2",
        params![now_ms, now_ms.saturating_sub(INACTIVITY_MS)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn scope_tasks(conn: &Connection, bot: &str, group: &str) -> Result<Vec<Task>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT task_id, owner_id, name, members_json FROM shared_tasks
         WHERE bot_id=?1 AND group_id=?2 AND ended_at_ms IS NULL ORDER BY task_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![bot, group], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut tasks = Vec::new();
    for row in rows {
        let (id, owner, name, json) = row.map_err(|e| e.to_string())?;
        let members = serde_json::from_str::<Vec<String>>(&json)
            .map_err(|e| format!("shared task members: {e}"))?;
        tasks.push(Task {
            id,
            owner,
            name,
            members,
        });
    }
    Ok(tasks)
}

fn save_members(conn: &Connection, task: &Task) -> Result<(), String> {
    let json = serde_json::to_string(&task.members).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE shared_tasks SET members_json=?1 WHERE task_id=?2",
        params![json, task.id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn reply(text: impl Into<String>, active: Option<&Task>) -> TaskRoute {
    TaskRoute {
        reply: Some(text.into()),
        task_id: active.map(|t| t.id.clone()),
        owner_id: active.map(|t| t.owner.clone()),
        ended_task_id: None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Create(&'a str),
    Join(&'a str),
    Status,
    Leave,
    End(&'a str),
    Usage,
}

fn code_valid(code: &str) -> bool {
    code.len() == 8
        && code
            .bytes()
            .all(|c| b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(&c))
}

fn command(text: &str) -> Option<Command<'_>> {
    let text = text.trim();
    match text {
        "任务状态" => return Some(Command::Status),
        "退出任务" => return Some(Command::Leave),
        "创建共享任务" | "加入任务" | "结束任务" => return Some(Command::Usage),
        _ => {}
    }
    // Full-line grammar, not substring/regex intent detection. Multiline prose stays prose.
    if text.contains(['\n', '\r']) {
        return None;
    }
    if let Some(name) = text.strip_prefix("创建共享任务 ") {
        let name = name.trim();
        return Some(if name.is_empty() || name.chars().count() > 80 {
            Command::Usage
        } else {
            Command::Create(name)
        });
    }
    for (prefix, end) in [("加入任务 ", false), ("结束任务 ", true)] {
        if let Some(code) = text.strip_prefix(prefix) {
            if code_valid(code) {
                return Some(if end {
                    Command::End(code)
                } else {
                    Command::Join(code)
                });
            }
            // Invalid single CODE-shaped ASCII tokens can get usage; ordinary prose
            // (including a sentence without spaces) must continue to the model.
            return if code.len() == 8 && code.bytes().all(|c| c.is_ascii_alphanumeric()) {
                Some(Command::Usage)
            } else {
                None
            };
        }
    }
    None
}

/// DM ingress can recognize commands without calling the group-only DB router.
pub fn is_task_command(text: &str) -> bool {
    command(text).is_some()
}

fn new_code(conn: &Connection) -> Result<String, String> {
    // 32 symbols / 40 random bits. OS CSPRNG; global primary key + collision retry.
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Rejection sampling also remains unbiased if this alphabet changes.
    for _ in 0..128 {
        let mut code = String::with_capacity(8);
        while code.len() < 8 {
            let mut byte = [0u8; 1];
            rand::rngs::OsRng
                .try_fill_bytes(&mut byte)
                .map_err(|e| e.to_string())?;
            if (byte[0] as usize) < (256 / ALPHABET.len()) * ALPHABET.len() {
                code.push(ALPHABET[byte[0] as usize % ALPHABET.len()] as char);
            }
        }
        let exists: Option<i32> = conn
            .query_row(
                "SELECT 1 FROM shared_tasks WHERE task_id=?1",
                [&code],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if exists.is_none() {
            return Ok(code);
        }
    }
    Err("任务码生成繁忙，请重试".into())
}

/// `reply=Some` consumes a command; otherwise task_id/owner_id route the actual message.
/// Join/create never silently replace an existing binding. Status/helpers do not touch TTL.
/// Group and bot are exact identities, not inferred from user IDs or task codes.
pub fn route(
    conn: &Connection,
    bot: &str,
    group: &str,
    user: &str,
    text: &str,
    now_ms: i64,
) -> Result<TaskRoute, String> {
    if bot.is_empty() || group.is_empty() || user.is_empty() {
        return Ok(reply(
            "共享任务仅在群聊中使用，请在群聊发送任务指令。",
            None,
        ));
    }
    let tx = begin(conn)?;
    expire(&tx, now_ms)?;
    let mut tasks = scope_tasks(&tx, bot, group)?;
    let current = tasks
        .iter()
        .position(|t| t.members.iter().any(|m| m == user));
    let active = current.map(|i| &tasks[i]);
    let result = match command(text) {
        Some(Command::Usage) => reply(
            "指令：创建共享任务 名称（1–80字） / 加入任务 CODE / 任务状态 / 退出任务 / 结束任务 CODE", active),
        Some(Command::Status) => match active {
            Some(t) => reply(format!("共享任务：{}\n任务码：{}\n创建者：{}\n成员：{}\n连续24小时无消息自动结束。",
                t.name, t.id, t.owner, t.members.join("、")), Some(t)),
            None => reply("当前为个人会话，尚未加入共享任务。", None),
        },
        Some(Command::Create(name)) => {
            if active.is_some() {
                reply("你已在共享任务中，请先发送「退出任务」。", active)
            } else {
                let count: i64 = tx.query_row("SELECT COUNT(*) FROM shared_tasks WHERE ended_at_ms IS NULL", [], |r| r.get(0))
                    .map_err(|e| e.to_string())?;
                if count >= MAX_ACTIVE_TASKS as i64 {
                    reply("活动任务已达200个，请先结束闲置任务。", None)
                } else {
                    let task = Task { id: new_code(&tx)?, owner: user.into(), name: name.into(), members: vec![user.into()] };
                    let members = serde_json::to_string(&task.members).map_err(|e| e.to_string())?;
                    tx.execute("INSERT INTO shared_tasks(task_id,bot_id,group_id,owner_id,name,members_json,created_at_ms,touched_at_ms)
                        VALUES(?1,?2,?3,?4,?5,?6,?7,?7)", params![task.id,bot,group,user,name,members,now_ms])
                        .map_err(|e| e.to_string())?;
                    reply(format!("已创建并加入共享任务「{}」。\n任务码：{}\n同群成员可发送「加入任务 {}」。", name, task.id, task.id), Some(&task))
                }
            }
        }
        Some(Command::Join(code)) => {
            match tasks.iter().position(|t| t.id == code) {
                None => reply("任务不存在、已结束或不属于当前机器人及群聊。", active),
                Some(i) if current == Some(i) => reply("你已加入该共享任务。", active),
                Some(_) if current.is_some() => reply("你已在其他共享任务中，请先发送「退出任务」。", active),
                Some(i) if tasks[i].members.len() >= MAX_MEMBERS => reply("该任务成员已达32人。", active),
                Some(i) => {
                    tasks[i].members.push(user.into());
                    save_members(&tx, &tasks[i])?;
                    reply(format!("已加入共享任务「{}」（{}）。", tasks[i].name, code), Some(&tasks[i]))
                }
            }
        }
        Some(Command::Leave) => match current {
            None => reply("当前为个人会话，尚未加入共享任务。", None),
            Some(i) => {
                tasks[i].members.retain(|m| m != user);
                save_members(&tx, &tasks[i])?;
                reply("已退出共享任务，后续消息恢复个人会话。", None)
            }
        },
        Some(Command::End(code)) => match tasks.iter().find(|t| t.id == code) {
            None => reply("任务不存在、已结束或不属于当前机器人及群聊。", active),
            Some(t) if t.owner != user => reply("仅任务创建者可结束该共享任务。", active),
            Some(t) => {
                tx.execute("UPDATE shared_tasks SET ended_at_ms=?1, members_json='[]' WHERE task_id=?2",
                    params![now_ms, t.id]).map_err(|e| e.to_string())?;
                // Command response describes the ended task, never another live binding.
                let mut r = reply("共享任务已结束，所有成员已解除该任务绑定。", None);
                r.ended_task_id = Some(t.id.clone());
                // Keep ended owner for cancellation/cleanup even when their binding is gone.
                r.owner_id = Some(t.owner.clone());
                r
            }
        },
        None => {
            if let Some(t) = active {
                if !text.trim().is_empty() {
                    tx.execute("UPDATE shared_tasks SET touched_at_ms=MAX(touched_at_ms,?1) WHERE task_id=?2",
                        params![now_ms, t.id]).map_err(|e| e.to_string())?;
                }
            }
            TaskRoute { reply: None, task_id: active.map(|t| t.id.clone()),
                owner_id: active.map(|t| t.owner.clone()), ended_task_id: None }
        }
    };
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

/// Resolve the current binding for control messages (stop / approval) without refreshing
/// inactivity. Expiry cleanup is persisted atomically; no message is consumed or recorded.
pub fn lookup_route(
    conn: &Connection,
    bot: &str,
    group: &str,
    user: &str,
    now_ms: i64,
) -> Result<TaskRoute, String> {
    let tx = begin(conn)?;
    expire(&tx, now_ms)?;
    let tasks = scope_tasks(&tx, bot, group)?;
    let active = tasks.iter().find(|t| t.members.iter().any(|m| m == user));
    let result = TaskRoute {
        reply: None,
        task_id: active.map(|t| t.id.clone()),
        owner_id: active.map(|t| t.owner.clone()),
        ended_task_id: None,
    };
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

/// Revalidate queued work immediately before execution. Does not refresh inactivity.
pub fn is_member(
    conn: &Connection,
    bot: &str,
    group: &str,
    task: &str,
    user: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let tx = begin(conn)?;
    expire(&tx, now_ms)?;
    let found = scope_tasks(&tx, bot, group)?
        .iter()
        .any(|t| t.id == task && t.members.iter().any(|m| m == user));
    tx.commit().map_err(|e| e.to_string())?;
    Ok(found)
}

pub fn active_count(conn: &Connection, bot: &str, now_ms: i64) -> Result<usize, String> {
    let tx = begin(conn)?;
    expire(&tx, now_ms)?;
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM shared_tasks WHERE bot_id=?1 AND ended_at_ms IS NULL",
            [bot],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_schema(&c).unwrap();
        ensure_schema(&c).unwrap();
        c
    }
    fn r(c: &Connection, user: &str, text: &str, time: i64) -> TaskRoute {
        route(c, "bot", "group", user, text, time).unwrap()
    }
    fn create(c: &Connection, user: &str) -> String {
        r(c, user, "创建共享任务 排障", 0).task_id.unwrap()
    }
    #[test]
    fn create_join_route_leave_and_end() {
        let c = db();
        let id = create(&c, "a");
        assert!(code_valid(&id));
        assert_eq!(
            r(&c, "b", &format!("加入任务 {id}"), 1).task_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(r(&c, "b", "正常消息", 2).owner_id.as_deref(), Some("a"));
        let status = r(&c, "b", "任务状态", 3).reply.unwrap();
        assert!(status.contains("a、b"));
        assert!(r(&c, "b", &format!("结束任务 {id}"), 4)
            .ended_task_id
            .is_none());
        assert!(r(&c, "b", "退出任务", 5).task_id.is_none());
        assert!(r(&c, "b", "你好", 6).task_id.is_none());
        r(&c, "c", &format!("加入任务 {id}"), 6);
        let end = r(&c, "a", &format!("结束任务 {id}"), 7);
        assert_eq!(end.ended_task_id, Some(id.clone()));
        assert_eq!(end.owner_id.as_deref(), Some("a"));
        assert!(end.task_id.is_none());
        assert!(!is_member(&c, "bot", "group", &id, "a", 8).unwrap());
        assert!(r(&c, "a", "你好", 8).task_id.is_none());
        assert!(r(&c, "c", "你好", 8).task_id.is_none());
        let members: String = c
            .query_row("SELECT members_json FROM shared_tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(members, "[]");
    }
    #[test]
    fn scopes_are_bound_and_private_commands_do_not_create() {
        let c = db();
        let id = create(&c, "a");
        let join = format!("加入任务 {id}");
        for (bot, group) in [("other", "group"), ("bot", "other"), ("bot", "")] {
            let got = route(&c, bot, group, "b", &join, 1).unwrap();
            assert!(got.task_id.is_none());
            assert!(!is_member(&c, bot, group, &id, "b", 1).unwrap());
        }
        assert!(route(&c, "bot", "", "a", "创建共享任务 foo", 1)
            .unwrap()
            .task_id
            .is_none());
        assert_eq!(active_count(&c, "bot", 1).unwrap(), 1);
    }
    #[test]
    fn inactivity_expiry_is_persistent_and_status_does_not_touch() {
        let c = db();
        let id = create(&c, "a");
        r(&c, "b", &format!("加入任务 {id}"), 100);
        r(&c, "a", "任务状态", INACTIVITY_MS - 1);
        assert!(is_member(&c, "bot", "group", &id, "a", INACTIVITY_MS - 1).unwrap());
        assert!(r(&c, "a", "hello", INACTIVITY_MS).task_id.is_none());
        assert!(!is_member(&c, "bot", "group", &id, "b", INACTIVITY_MS).unwrap());
        assert_eq!(active_count(&c, "bot", INACTIVITY_MS).unwrap(), 0);
        assert!(r(&c, "a", &format!("加入任务 {id}"), INACTIVITY_MS + 1)
            .task_id
            .is_none());
    }
    #[test]
    fn control_lookup_preserves_ttl_and_expires_binding() {
        let c = db();
        let id = create(&c, "owner");
        r(&c, "guest", &format!("加入任务 {id}"), 1);
        for time in [2, 100, INACTIVITY_MS - 1] {
            let lookup = lookup_route(&c, "bot", "group", "guest", time).unwrap();
            assert_eq!(lookup.task_id.as_deref(), Some(id.as_str()));
            assert_eq!(lookup.owner_id.as_deref(), Some("owner"));
            assert!(lookup.reply.is_none());
            assert!(lookup.ended_task_id.is_none());
        }
        let touched: i64 = c
            .query_row(
                "SELECT touched_at_ms FROM shared_tasks WHERE task_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(touched, 0);
        assert_eq!(
            lookup_route(&c, "bot", "group", "guest", INACTIVITY_MS).unwrap(),
            TaskRoute::default()
        );
        let (ended, members): (Option<i64>, String) = c
            .query_row(
                "SELECT ended_at_ms,members_json FROM shared_tasks WHERE task_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ended, Some(INACTIVITY_MS));
        assert_eq!(members, "[]");
        assert_eq!(
            lookup_route(&c, "bot", "group", "owner", INACTIVITY_MS + 1).unwrap(),
            TaskRoute::default()
        );
    }

    #[test]
    fn control_lookup_is_exactly_scoped_and_has_no_side_effect_on_binding() {
        let c = db();
        let id = create(&c, "owner");
        for (bot, group, user) in [
            ("other", "group", "owner"),
            ("bot", "other", "owner"),
            ("bot", "group", "stranger"),
            ("bot", "", "owner"),
        ] {
            assert_eq!(
                lookup_route(&c, bot, group, user, 1).unwrap(),
                TaskRoute::default()
            );
        }
        assert_eq!(
            lookup_route(&c, "bot", "group", "owner", 1)
                .unwrap()
                .task_id,
            Some(id)
        );
        r(&c, "owner", "退出任务", 2);
        assert_eq!(
            lookup_route(&c, "bot", "group", "owner", 3).unwrap(),
            TaskRoute::default()
        );
    }

    #[test]
    fn only_real_nonempty_messages_extend_ttl() {
        let c = db();
        let id = create(&c, "a");
        r(&c, "a", "message", 500);
        r(&c, "a", " ", 1000);
        assert!(is_member(&c, "bot", "group", &id, "a", INACTIVITY_MS + 499).unwrap());
        assert!(!is_member(&c, "bot", "group", &id, "a", INACTIVITY_MS + 500).unwrap());
    }
    #[test]
    fn same_group_users_have_distinct_tasks_and_one_binding_each() {
        let c = db();
        let a = create(&c, "a");
        let b = create(&c, "b");
        assert_ne!(a, b);
        assert_eq!(r(&c, "a", "ordinary", 1).task_id, Some(a.clone()));
        assert_eq!(r(&c, "b", "ordinary", 1).task_id, Some(b.clone()));
        assert_eq!(
            r(&c, "a", &format!("加入任务 {b}"), 2).task_id,
            Some(a.clone())
        );
        assert_eq!(r(&c, "a", "创建共享任务 another", 2).task_id, Some(a));
        assert_eq!(active_count(&c, "bot", 2).unwrap(), 2);
        r(&c, "a", "退出任务", 3);
        assert_eq!(r(&c, "a", &format!("加入任务 {b}"), 4).task_id, Some(b));
    }
    #[test]
    fn limits_and_expiry_release_capacity() {
        let c = db();
        let id = create(&c, "owner");
        for n in 0..31 {
            assert!(r(&c, &format!("u{n}"), &format!("加入任务 {id}"), 1)
                .task_id
                .is_some());
        }
        assert!(r(&c, "overflow", &format!("加入任务 {id}"), 2)
            .task_id
            .is_none());
        for n in 1..MAX_ACTIVE_TASKS {
            create(&c, &format!("creator{n}"));
        }
        assert!(r(&c, "overflow", "创建共享任务 overflow", 2)
            .task_id
            .is_none());
        assert_eq!(active_count(&c, "bot", 2).unwrap(), 200);
        assert!(
            r(&c, "overflow", "创建共享任务 after-expiry", INACTIVITY_MS)
                .task_id
                .is_some()
        );
        assert_eq!(active_count(&c, "bot", INACTIVITY_MS).unwrap(), 1);
    }
    #[test]
    fn full_line_commands_only() {
        for text in [
            "请创建共享任务 foo",
            "任务状态是什么",
            "退出任务后继续",
            "加入任务 ABCDEFGH 然后分析",
            "结束任务 ABCDEFGH\n继续",
            "结束任务 会影响什么",
            "加入任务 后怎么退出",
            "解释结束任务 CODE",
        ] {
            assert!(!is_task_command(text), "{text}");
        }
        assert!(is_task_command("任务状态"));
        assert!(is_task_command("加入任务 ABCDEFGH"));
    }
    #[test]
    fn failed_write_rolls_back_the_transaction() {
        let c = db();
        let id = create(&c, "owner");
        c.execute_batch(
            "CREATE TRIGGER block_join BEFORE UPDATE OF members_json ON shared_tasks
            BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
        assert!(route(&c, "bot", "group", "guest", &format!("加入任务 {id}"), 1).is_err());
        c.execute_batch("DROP TRIGGER block_join").unwrap();
        assert!(!is_member(&c, "bot", "group", &id, "guest", 2).unwrap());
        assert!(is_member(&c, "bot", "group", &id, "owner", 2).unwrap());
    }

    #[test]
    fn creator_can_end_after_leaving_without_disrupting_another_binding() {
        let c = db();
        let first = create(&c, "owner");
        r(&c, "owner", "退出任务", 1);
        let second = create(&c, "other");
        r(&c, "owner", &format!("加入任务 {second}"), 2);
        let end = r(&c, "owner", &format!("结束任务 {first}"), 3);
        assert_eq!(end.ended_task_id, Some(first));
        assert_eq!(end.owner_id.as_deref(), Some("owner"));
        assert_eq!(r(&c, "owner", "ordinary", 4).task_id, Some(second));
    }

    #[test]
    fn metadata_survives_connection_restart() {
        let path = std::env::temp_dir().join(format!(
            "wecom-tasks-{}-{}.db",
            std::process::id(),
            rand::random::<u64>()
        ));
        let id;
        {
            let c = Connection::open(&path).unwrap();
            ensure_schema(&c).unwrap();
            id = create(&c, "a");
            r(&c, "b", &format!("加入任务 {id}"), 1);
        }
        {
            let c = Connection::open(&path).unwrap();
            ensure_schema(&c).unwrap();
            assert_eq!(r(&c, "b", "hello", 2).task_id, Some(id.clone()));
            r(&c, "a", &format!("结束任务 {id}"), 3);
        }
        {
            let c = Connection::open(&path).unwrap();
            assert!(r(&c, "b", "hello", 4).task_id.is_none());
        }
        std::fs::remove_file(path).unwrap();
    }
}
