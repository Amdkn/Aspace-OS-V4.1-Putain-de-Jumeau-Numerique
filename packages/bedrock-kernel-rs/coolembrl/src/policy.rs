//! Adaptateur du policy store L05 (design Phase 2, section 0.3 ; PRD section 6).
//!
//! Le runtime ne contient AUCUNE regle de politique, ni meme le vocabulaire
//! du store : il LIT le store au moment de la decision (REGISTRY.md +
//! CHANGELOG.md du store L05), y compris le jeton par lequel le registre
//! declare une politique en vigueur (la phrase de definition du REGISTRY
//! le porte). Il epingle la version gouvernante dans l'enveloppe emise
//! (policy_ref) et refuse fail-closed si le store est absent, illisible
//! ou hors vigueur. Changer une politique au store change les decisions
//! sans toucher a ce code (H-P2-11).

use std::path::Path;

#[derive(Debug, Clone)]
pub struct PolicyEntry {
    pub id: String,
    pub status: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct PolicySnapshot {
    pub store_version: String,
    pub in_force_token: String,
    pub policies: Vec<PolicyEntry>,
}

impl PolicySnapshot {
    /// Le store est en vigueur si chaque politique inscrite porte le jeton
    /// de vigueur que le REGISTRY definit lui-meme.
    pub fn in_force(&self) -> bool {
        !self.policies.is_empty()
            && self
                .policies
                .iter()
                .all(|p| p.status == self.in_force_token)
    }

    /// Reference epinglee dans l'enveloppe : la decision est reproductible
    /// contre la version exacte qui l'a gouvernee.
    pub fn policy_ref(&self) -> String {
        let parts: Vec<String> = self
            .policies
            .iter()
            .map(|p| format!("{}@{}", p.id, p.version))
            .collect();
        format!("L05@{}[{}]", self.store_version, parts.join(","))
    }
}

pub fn load_policy_store(dir: &Path) -> Result<PolicySnapshot, String> {
    let registry = std::fs::read_to_string(dir.join("REGISTRY.md"))
        .map_err(|e| format!("REGISTRY.md illisible: {e}"))?;
    let changelog = std::fs::read_to_string(dir.join("CHANGELOG.md"))
        .map_err(|e| format!("CHANGELOG.md illisible: {e}"))?;
    let store_version = extract_store_version(&changelog)
        .ok_or_else(|| "version du store introuvable au CHANGELOG".to_string())?;
    let in_force_token = extract_in_force_token(&registry)
        .ok_or_else(|| "jeton de vigueur introuvable au REGISTRY (phrase de definition du statut)".to_string())?;
    let mut policies = Vec::new();
    for line in registry.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        // | [P-XXX](POLICY-XXX.md) | enonce | STATUT | enforcement | vN - date |
        if cells.len() < 6 {
            continue;
        }
        let id_cell = cells[1];
        let Some(start) = id_cell.find("P-") else { continue };
        let id: String = id_cell[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if id.len() < 3 || !id.starts_with("P-") {
            continue;
        }
        let status = cells[3].to_string();
        let version = cells[5]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '·' || c == '|')
            .to_string();
        if version.is_empty() {
            continue;
        }
        policies.push(PolicyEntry {
            id,
            status,
            version,
        });
    }
    if policies.is_empty() {
        return Err("aucune politique lue au REGISTRY".to_string());
    }
    policies.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(PolicySnapshot {
        store_version,
        in_force_token,
        policies,
    })
}

/// Le REGISTRY definit lui-meme le statut de vigueur, dans sa phrase
/// liminaire : « Statut d'une politique : **JETON** (enforcement ...) ».
/// Le jeton est lu la, jamais code en dur.
fn extract_in_force_token(registry: &str) -> Option<String> {
    let marker = "Statut d'une politique";
    let pos = registry.find(marker)?;
    let rest = &registry[pos..];
    let start = rest.find("**")? + 2;
    let end = rest[start..].find("**")? + start;
    let token = rest[start..end].trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn extract_store_version(changelog: &str) -> Option<String> {
    // Premiere occurrence d'un semver vX.Y.Z dans le CHANGELOG (la plus recente
    // entree est en tete de fichier).
    let bytes = changelog.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'v' {
            let rest = &changelog[i + 1..];
            let token: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
                return Some(format!("v{token}"));
            }
        }
        i += 1;
    }
    None
}
