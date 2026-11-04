<img src="assets/icon.svg" width="64" height="64" alt="">

# Calepin

Bloc-notes natif et rapide pour Linux : onglets jamais perdus, Markdown rendu
pendant la frappe, tableaux, minimap. Pensé pour la lecture rapide, la prise de
notes et l'édition rapide, pas comme un IDE. GTK4 + GtkSourceView 5, en Rust,
sous licence MIT.

Il s'intègre à [Omarchy](https://omarchy.org) (thème et police suivis à chaud,
bloc-notes sur SUPER+N), mais fonctionne sur tout bureau Linux récent.

## Installer

### Flatpak (toutes distributions)

```bash
flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08 org.flatpak.Builder
flatpak run org.flatpak.Builder --user --install --force-clean build-dir \
  flatpak/io.github.antoineart.Calepin.yml
flatpak run io.github.antoineart.Calepin
```

Le Flatpak apporte ses propres GTK et GtkSourceView : il tourne aussi sur des
distributions anciennes (Debian 12, Ubuntu 22.04).

### Depuis les sources

Prérequis : GTK ≥ 4.14, GtkSourceView ≥ 5.10 (Arch, Fedora ≥ 40,
Ubuntu ≥ 24.04…) et Rust ≥ 1.88 ([rustup](https://rustup.rs) si celui de la
distribution est plus ancien).

```bash
# Arch :          sudo pacman -S gtk4 gtksourceview5 rust
# Fedora :        sudo dnf install gtk4-devel gtksourceview5-devel
# Debian/Ubuntu : sudo apt install build-essential libgtk-4-dev libgtksourceview-5-dev
./install.sh   # binaire dans ~/.local/bin, entrée .desktop (« Ouvrir avec »), icône
```

## Utilisation

- `calepin` ouvre la fenêtre, `calepin fichier.md` ouvre le fichier dans
  l'instance déjà lancée (une seule instance), `calepin --new` ouvre un onglet
  vide, `calepin --background` charge tout sans fenêtre (lancement de session).
- Fermer la fenêtre la cache seulement : le processus reste résident pour
  s'afficher instantanément. Ctrl+Q quitte vraiment.
- Chaque onglet est enregistré en continu (`~/.local/state/calepin/`), même une
  note jamais enregistrée dans un fichier : fermer la fenêtre, éteindre ou
  tuer le processus ne perd rien. Ctrl+Maj+T rouvre les 30 derniers onglets
  fermés.
- Onglets : rien pour une note jamais enregistrée ; une icône de fichier pour
  un fichier enregistré ; l'icône en couleur d'accent et ● pour un fichier
  modifié non enregistré. Fermer une note non enregistrée demande confirmation.
- Les `.md` s'ouvrent en mode riche, tout autre fichier en texte brut avec
  coloration syntaxique. Un fichier binaire ou non UTF-8 s'ouvre en lecture
  seule.
- Tirer un onglet hors de la barre d'onglets (sur le texte ou hors de la
  fenêtre) l'ouvre dans une fenêtre à part ; le lâcher sur une barre d'onglets
  le déplace ou le réordonne. Ctrl+Maj+D au clavier.
- Thème : sur Omarchy, les couleurs du thème courant et la police monospace,
  suivies à chaud. Ailleurs, le style clair ou sombre et la couleur d'accent du
  bureau (portail XDG), suivis à chaud aussi.
- Barre de titre : sous un gestionnaire en mosaïque (Hyprland, Sway, niri, i3…)
  aucune ; ailleurs, la barre d'onglets sert de barre de titre.
  `CALEPIN_DECORATIONS=1` ou `0` force le choix.

## Saisie Markdown

La syntaxe disparaît dès qu'elle est complète : `# ` `## ` `### ` titres,
`- ` puces, `1. ` liste numérotée, `[] ` case à cocher, `> ` citation,
` ``` ` + Entrée bloc de code, `**gras**`, `*italique*`, `~~barré~~`,
`` `code` ``, `[texte](url)`, URL nues.

Tableaux : `| a | b |`, Entrée, `|---|---|`, Entrée (ou le bouton ▦,
Ctrl+Alt+T). Tab et Maj+Tab passent de cellule en cellule, Tab sur la dernière
cellule ajoute une ligne, Échap sort du tableau. Le texte des cellules passe à
la ligne ; « ⇔ Élargir » laisse un tableau dépasser la colonne de texte.

Entrée sur un élément de liste vide sort de la liste ; Retour arrière au début
d'un titre ou d'une citation le remet en texte ; Tab et Maj+Tab indentent les
listes. Clic sur une case pour la cocher, Ctrl+clic pour ouvrir un lien.
Copier-coller passent par du Markdown ; Ctrl+Maj+V colle le texte brut.
Ctrl+Z annule texte et mise en forme ensemble (l'historique garde le Markdown).

## Raccourcis

| Action | Touches |
|---|---|
| Gras, italique, barré, code | Ctrl+B, Ctrl+I, Ctrl+Maj+X, Ctrl+E |
| Lien | Ctrl+K |
| Texte, titres 1-3 | Ctrl+Alt+0, Ctrl+Alt+1..3 |
| Case, puces, numéros, code, citation | Ctrl+Alt+4, 5, 6, 8, Ctrl+Alt+Q |
| Tableau | Ctrl+Alt+T |
| Effacer la mise en forme | Ctrl+\ |
| Annuler, rétablir | Ctrl+Z, Ctrl+Maj+Z (ou Ctrl+Y) |
| Nouvel onglet, fermer, rouvrir | Ctrl+T (ou Ctrl+N), Ctrl+W, Ctrl+Maj+T |
| Détacher l'onglet | Ctrl+Maj+D (ou glisser l'onglet hors de la barre) |
| Onglet suivant / précédent | Ctrl+Tab, Ctrl+Maj+Tab, Ctrl+PgSuiv/PgPréc, Alt+1..9 |
| Déplacer l'onglet | Ctrl+Maj+PgSuiv/PgPréc |
| Ouvrir, enregistrer, enregistrer sous | Ctrl+O, Ctrl+S, Ctrl+Maj+S |
| Rechercher (casse et accents ignorés) | Ctrl+F, Entrée / Maj+Entrée, F3, Échap |
| Minimap, barre d'outils, source Markdown | Ctrl+M, Ctrl+Maj+F, Ctrl+Maj+E |
| Zoom (retenu) | Ctrl+molette, Ctrl++, Ctrl+-, Ctrl+0 |
| Quitter | Ctrl+Q |

## Omarchy et Hyprland

`extras/omarchy/` contient de quoi faire de Calepin le bloc-notes de SUPER+N :

- `calepin-notes-toggle` : à copier dans `~/.local/bin`, montre ou cache les
  notes sur l'espace spécial `notes` ;
- `bindings.lua`, `windows.lua`, `autostart.lua` : les lignes à ajouter dans
  les fichiers du même nom de `~/.config/hypr/` (raccourci, fenêtre flottante
  et haute, lancement caché à l'ouverture de session).

La fenêtre des notes se reconnaît à son titre, qui commence par « Calepin » ;
les fenêtres d'onglets détachés finissent par « — Calepin ».

## Développement

```bash
cargo test                  # modèle Markdown, recherche
cargo build --release
CALEPIN_TIMING=1 calepin    # étapes du démarrage
```

Après un changement de dépendance, régénérer `flatpak/cargo-sources.json` avec
[`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
