use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Accounts {
    users: Table,
    groups: Table,
}

#[derive(Debug, Clone, Default)]
struct Table {
    ids: HashMap<String, u32>,
    names: HashMap<u32, String>,
}

impl Table {
    fn parse(text: &str) -> Self {
        let mut table = Table::default();
        for line in text.lines() {
            let mut fields = line.split(':');
            let (Some(name), Some(_), Some(id)) = (fields.next(), fields.next(), fields.next()) else { continue };
            let Ok(id) = id.trim().parse::<u32>() else { continue };
            if name.is_empty() || name.starts_with(['#', '+', '-']) {
                continue;
            }
            table.ids.entry(name.to_owned()).or_insert(id);
            table.names.entry(id).or_insert_with(|| name.to_owned());
        }
        table
    }
}

impl Accounts {
    pub fn load() -> Self {
        #[cfg(unix)]
        {
            let read = |path: &str| std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
            Accounts::parse(&read("/etc/passwd"), &read("/etc/group"))
        }
        #[cfg(not(unix))]
        {
            Accounts::default()
        }
    }

    pub fn parse(passwd: &str, group: &str) -> Self {
        Accounts { users: Table::parse(passwd), groups: Table::parse(group) }
    }

    pub fn user_id(&self, name: &str) -> Option<u32> {
        self.users.ids.get(name).copied()
    }

    pub fn group_id(&self, name: &str) -> Option<u32> {
        self.groups.ids.get(name).copied()
    }

    pub fn user_name(&self, id: u32) -> Option<&str> {
        self.users.names.get(&id).map(String::as_str)
    }

    pub fn group_name(&self, id: u32) -> Option<&str> {
        self.groups.names.get(&id).map(String::as_str)
    }
}
