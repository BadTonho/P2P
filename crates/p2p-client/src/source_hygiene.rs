//! Guarda de higiene do código-fonte.
//!
//! Impede a reintrodução de defeitos de codificação que compilam e passam
//! despercebidos em revisão, busca e diffs:
//!
//! * caracteres invisíveis (U+00AD, faixas de formatação bidirecional,
//!   separadores) dentro de literais ou identificadores;
//! * texto com codificação dupla (UTF-8 relido como CP-1252 e regravado em
//!   UTF-8), detectado pela letra A maiúscula com til ou circunflexo seguida
//!   de letra decorada, que nunca é legítima em texto em português e não
//!   aparece em identificadores.
//!
//! Qualquer ocorrência quebra o teste com a lista completa dos achados.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::{collect_rust_files, examine_file};
    use std::path::Path;

    #[test]
    fn fonte_nao_contem_caracteres_invisiveis_nem_texto_com_codificacao_dupla() {
        let crate_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let files = collect_rust_files(&crate_directory);
        assert!(
            files.len() > 50,
            "esperava varrer o código-fonte completo e encontrei {} arquivos;\
             a busca do teste precisa acompanhar a estrutura de diretórios",
            files.len()
        );

        let mut violations = Vec::new();
        for file in &files {
            violations.extend(examine_file(file));
        }

        assert!(
            violations.is_empty(),
            "caracteres de codificação problemáticos encontrados no código-fonte:\n{}",
            violations
                .iter()
                .map(|violation| format!("{violation}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// Caracteres proibidos no código-fonte.
///
/// * `0x00AD`: hífen suave invisível (mesma face visual que o hífen normal);
/// * `0x200B..=0x200F`: zero-width e marcas bidirecionais;
/// * `0x202A..=0x202E` e `0x2060..=0x2066`: formatação bidirecional e
///   anexadores invisíveis;
/// * `0xFEFF`: byte order mark no meio do arquivo;
/// * `0x0085`: quebra de linha invisível (NEL) que quebra a numeração de
///   linhas em editores.
fn is_forbidden(character: char) -> bool {
    matches!(character, '\u{AD}' | '\u{85}' | '\u{FEFF}')
        || ('\u{200B}'..='\u{200F}').contains(&character)
        || ('\u{202A}'..='\u{202E}').contains(&character)
        || ('\u{2060}'..='\u{2066}').contains(&character)
}

/// Letra maiúscula acentuada que nunca abre palavra em português e não faz
/// parte de identificadores do projeto: assinatura clássica de texto salvo
/// duas vezes com codificações diferentes.
fn is_double_encoded_signature(character: char) -> bool {
    matches!(character, '\u{C2}' | '\u{C3}')
}

fn collect_rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn examine_file(path: &Path) -> Vec<Violation> {
    let Ok(content) = fs::read(path) else {
        return vec![Violation::new(
            path,
            1,
            "arquivo não pôde ser lido".to_owned(),
        )];
    };
    let text = String::from_utf8_lossy(&content);
    let mut violations = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let mut double_encoded = false;
        for (column, character) in line.char_indices() {
            if is_forbidden(character) {
                violations.push(Violation::new(
                    path,
                    line_index + 1,
                    format!(
                        "caractere invisível U+{:04X} na coluna {}",
                        character as u32,
                        column + 1
                    ),
                ));
            } else if is_double_encoded_signature(character) {
                double_encoded = true;
            }
        }
        if double_encoded {
            violations.push(Violation::new(
                path,
                line_index + 1,
                "texto com suspeita de codificação dupla (A maiúscula com til ou circunflexo seguida de letra decorada)".to_owned(),
            ));
        }
    }
    violations
}

struct Violation {
    path: PathBuf,
    line: usize,
    detail: String,
}

impl Violation {
    fn new(path: &Path, line: usize, detail: String) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            detail,
        }
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}: {}",
            self.path.display(),
            self.line,
            self.detail
        )
    }
}
