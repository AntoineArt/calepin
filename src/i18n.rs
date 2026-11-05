//! Interface language: English, or French when the user's locale is French.
//! The translations live in the binary, so the self-updating single binary
//! needs no data files.

use std::fmt::Display;
use std::sync::OnceLock;

/// The locale variables, in the order gettext reads them.
fn locale() -> String {
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let first = v.split(':').next().unwrap_or("").trim().to_string();
            if !first.is_empty() {
                return first;
            }
        }
    }
    String::new()
}

fn french() -> bool {
    static FR: OnceLock<bool> = OnceLock::new();
    *FR.get_or_init(|| locale().starts_with("fr"))
}

/// The text in the user's language (English when no translation exists).
pub fn tr(en: &str) -> &str {
    if french() {
        if let Some((_, fr)) = FR.iter().find(|(e, _)| *e == en) {
            return fr;
        }
    }
    en
}

/// The singular or plural form for `n` (French uses the singular for 0).
pub fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 || (n == 0 && french()) { one } else { many }
}

/// `tr`, then each `{}` replaced by the next argument.
pub fn trf(en: &str, args: &[&dyn Display]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut parts = tr(en).split("{}").peekable();
    while let Some(p) = parts.next() {
        out.push_str(p);
        if parts.peek().is_some() {
            if let Some(a) = args.next() {
                out.push_str(&a.to_string());
            }
        }
    }
    out
}

/// English, then French. Keys are the English texts used in the code.
const FR: &[(&str, &str)] = &[
    // Tabs
    ("New note", "Nouvelle note"),
    ("Note not saved to a file (kept by Calepin)", "Note non enregistrée dans un fichier (conservée par Calepin)"),
    ("{}\nModified, not saved (Ctrl+S)", "{}\nModifié, non enregistré (Ctrl+S)"),
    ("{}\nSaved", "{}\nEnregistré"),
    ("New tab (Ctrl+T)", "Nouvel onglet (Ctrl+T)"),
    ("Close (Ctrl+W)", "Fermer (Ctrl+W)"),
    // Files
    ("\n\n… (file truncated: too large to edit)", "\n\n… (fichier tronqué : trop gros pour être édité)"),
    ("This file is not UTF-8 text: it is open read-only.", "Ce fichier n'est pas du texte UTF-8 : il est ouvert en lecture seule."),
    ("Cannot save: {}", "Impossible d'enregistrer : {}"),
    // Closing
    ("Save “{}” before closing?", "Enregistrer « {} » avant de fermer ?"),
    (
        "This note was never saved to a file. Closed without saving, it can still be reopened with Ctrl+Shift+T.",
        "Cette note n'a jamais été enregistrée dans un fichier. Fermée sans enregistrer, elle reste récupérable avec Ctrl+Maj+T.",
    ),
    (
        "The changes are not saved to the file. Closed without saving, the tab can still be reopened with Ctrl+Shift+T.",
        "Les modifications ne sont pas enregistrées dans le fichier. Fermé sans enregistrer, l'onglet reste récupérable avec Ctrl+Maj+T.",
    ),
    ("A tab is not saved to a file", "Un onglet n'est pas enregistré dans un fichier"),
    ("{} tabs are not saved to a file", "{} onglets ne sont pas enregistrés dans un fichier"),
    (
        "Move to main window keeps every tab in the main Calepin window. Closed tabs can still be reopened with Ctrl+Shift+T.",
        "« Remettre dans la fenêtre principale » y garde tous les onglets. Fermés, ils restent récupérables avec Ctrl+Maj+T.",
    ),
    ("Cancel", "Annuler"),
    ("Close without saving", "Fermer sans enregistrer"),
    ("Move to main window", "Remettre dans la fenêtre principale"),
    ("Save", "Enregistrer"),
    ("Save…", "Enregistrer…"),
    // Find
    ("Find", "Rechercher"),
    ("Previous (Shift+Enter)", "Précédent (Maj+Entrée)"),
    ("Next (Enter)", "Suivant (Entrée)"),
    ("No results", "Aucun résultat"),
    // Toolbar
    ("{} word", "{} mot"),
    ("{} words", "{} mots"),
    ("Text", "Texte"),
    ("Normal text (Ctrl+Alt+0)", "Texte normal (Ctrl+Alt+0)"),
    ("Heading 1 (Ctrl+Alt+1)", "Titre 1 (Ctrl+Alt+1)"),
    ("Heading 2 (Ctrl+Alt+2)", "Titre 2 (Ctrl+Alt+2)"),
    ("Heading 3 (Ctrl+Alt+3)", "Titre 3 (Ctrl+Alt+3)"),
    ("Bold (Ctrl+B)", "Gras (Ctrl+B)"),
    ("Italic (Ctrl+I)", "Italique (Ctrl+I)"),
    ("Strikethrough (Ctrl+Shift+X)", "Barré (Ctrl+Maj+X)"),
    ("Code (Ctrl+E)", "Code (Ctrl+E)"),
    ("Link", "Lien"),
    ("Link (Ctrl+K)", "Lien (Ctrl+K)"),
    ("Bulleted list (Ctrl+Alt+5)", "Liste à puces (Ctrl+Alt+5)"),
    ("Numbered list (Ctrl+Alt+6)", "Liste numérotée (Ctrl+Alt+6)"),
    ("Checkbox (Ctrl+Alt+4)", "Case à cocher (Ctrl+Alt+4)"),
    ("Quote (Ctrl+Alt+Q)", "Citation (Ctrl+Alt+Q)"),
    ("Code block (Ctrl+Alt+8)", "Bloc de code (Ctrl+Alt+8)"),
    ("Table (Ctrl+Alt+T)", "Tableau (Ctrl+Alt+T)"),
    ("Clear formatting (Ctrl+\\)", "Effacer la mise en forme (Ctrl+\\)"),
    ("Rich", "Rendu"),
    ("Show the raw Markdown (Ctrl+Shift+E)", "Afficher le Markdown brut (Ctrl+Maj+E)"),
    ("Back to the rendered view (Ctrl+Shift+E)", "Revenir au rendu (Ctrl+Maj+E)"),
    ("https://… (empty to remove)", "https://… (vide pour retirer)"),
    // Tables
    ("Column {}", "Colonne {}"),
    ("⇔ Widen", "⇔ Élargir"),
    ("⇔ Narrow", "⇔ Réduire"),
    ("Let the table extend past the text column", "Laisser le tableau dépasser la colonne de texte"),
    ("+ row", "+ ligne"),
    ("+ column", "+ colonne"),
    ("− row", "− ligne"),
    ("− column", "− colonne"),
    ("Delete table", "Supprimer le tableau"),
    ("Add a row below the cell (Tab on the last cell)", "Ajouter une ligne sous la cellule (Tab sur la dernière cellule)"),
    ("Add a column to the right", "Ajouter une colonne à droite"),
    ("Delete the row", "Supprimer la ligne"),
    ("Delete the column", "Supprimer la colonne"),
    ("Delete the table", "Supprimer le tableau"),
    // Updates
    ("Calepin {} is installed. Restart to use it (your tabs are kept).", "Calepin {} est installé. Redémarrer pour l'utiliser (les onglets sont conservés)."),
    ("Calepin {} is available.", "Calepin {} est disponible."),
    ("Calepin {} is available, but the update failed: {}", "Calepin {} est disponible, mais la mise à jour a échoué : {}"),
    ("Restart", "Redémarrer"),
    ("View release", "Voir la version"),
    ("Cannot restart: {}", "Redémarrage impossible : {}"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_match() {
        for (en, fr) in FR {
            assert_eq!(en.matches("{}").count(), fr.matches("{}").count(), "{en}");
        }
        let mut keys: Vec<_> = FR.iter().map(|(e, _)| e).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), FR.len(), "duplicate key");
    }

    #[test]
    fn format() {
        assert_eq!(trf("Calepin {} is available, but the update failed: {}", &[&"1.0", &"x"]).matches("1.0").count(), 1);
    }
}
