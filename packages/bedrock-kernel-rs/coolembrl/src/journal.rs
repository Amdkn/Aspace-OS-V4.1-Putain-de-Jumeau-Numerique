//! Journal de boot append-only (etage 0 du recu, design Phase 1 section 0.2).
//! Une ligne JSON par evenement ; les lignes d'un boot portent son boot_id.
//! L'entree d'un boot n'est jamais reecrite par le boot suivant : la chaine
//! prev_boot_id lie, elle ne corrige pas. Chaque append est suivi d'un fsync :
//! le journal survit au kill -9 par construction (ecrit des S0).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

pub struct Journal {
    file: Mutex<File>,
    path: std::path::PathBuf,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("ouverture du journal de boot: {e}"))?;
        Ok(Journal {
            file: Mutex::new(file),
            path: path.to_path_buf(),
        })
    }

    pub fn append(&self, line: &serde_json::Value) {
        let mut s = line.to_string();
        s.push('\n');
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(s.as_bytes());
            let _ = f.sync_data();
        }
    }

    /// Lit la derniere ligne complete du journal (tous boots confondus).
    pub fn last_line(&self) -> Option<serde_json::Value> {
        let mut content = String::new();
        File::open(&self.path)
            .ok()?
            .read_to_string(&mut content)
            .ok()?;
        content
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| serde_json::from_str(l).ok())
    }

    /// Le boot precedent a-t-il laisse une entree sans ligne de cloture ?
    /// (detection de recuperation apres mort brutale, H-P1-4.)
    pub fn previous_boot_unterminated(&self, current_boot_id: &str) -> Option<String> {
        let mut content = String::new();
        File::open(&self.path)
            .ok()?
            .read_to_string(&mut content)
            .ok()?;
        let mut last_boot: Option<String> = None;
        let mut terminated: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for line in content.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(bid) = v.get("boot_id").and_then(|b| b.as_str()) else {
                continue;
            };
            if bid == current_boot_id {
                continue;
            }
            last_boot = Some(bid.to_string());
            if v.get("kind").and_then(|k| k.as_str()) == Some("receipt")
                || v.get("kind").and_then(|k| k.as_str()) == Some("shutdown")
            {
                terminated.insert(bid.to_string(), true);
            }
        }
        match last_boot {
            Some(bid) if terminated.get(&bid) != Some(&true) => Some(bid),
            _ => None,
        }
    }
}
