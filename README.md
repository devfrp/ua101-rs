# ua101-rs

Pilote **multi-plateforme en Rust** pour l'interface audio USB
**Roland / EDIROL UA-101**, dont Roland ne fournit plus de pilote à jour.

Il tourne entièrement en espace utilisateur et ne contient aucun code C.
Le protocole a été reconstitué par analyse statique du pilote Windows
d'origine, puis vérifié sur
les descripteurs d'une vraie carte. Voir [`docs/protocol.md`](docs/protocol.md).

## État

| Fonction                       | Linux | macOS | Windows |
|--------------------------------|:-----:|:-----:|:-------:|
| Détection, descripteurs        | ✅    | ✅    | ✅ (WinUSB) |
| Monitoring direct (expérimental) | ✅  | ✅    | ✅      |
| MIDI in/out                    | ✅    | ✅    | ✅      |
| Audio 10 sorties / 12 entrées  | ✅ (usbfs) | 🚧 IOKit isoch | 🚧 WinUSB isoch |
| Périphérique audio système (PipeWire/JACK, CoreAudio, ASIO/WASAPI) | 🚧 | 🚧 | 🚧 |

✅ implémenté · 🚧 à faire. Sous Linux, l'enregistrement 12 canaux a été
validé sur une vraie carte à 44,1 kHz. La lecture audible reste à
valider (voir *Questions ouvertes* dans `docs/protocol.md`).

## Installation

### Linux (toutes distributions)

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/ua101-rs/main/packaging/linux/install.sh | sh
```

Le script détecte l'architecture (x86_64, aarch64) et la libc (glibc ou
musl). Il installe les bibliothèques du panneau avec le gestionnaire de
paquets de la distribution (apt, dnf, pacman, zypper, xbps), vérifie le
SHA-256 de l'archive, puis installe `ua101`, `ua101-panel` et une règle
udev qui donne l'accès à la carte sans root.

Options : `sh -s -- --cli-only`, `--no-deps`, `--version v0.1.0`,
`--prefix ~/.local`, `--blacklist-kernel` (empêche le pilote noyau
`snd-ua101` de prendre la carte), `--uninstall`.

> Linux contient déjà un pilote noyau (`snd-ua101`). `ua101` le détache
> automatiquement des interfaces qu'il utilise, puis le rattache en sortant.

### Windows

Télécharge `ua101-panel-windows-x86_64.exe` (panneau) ou
`ua101-windows-x86_64.exe` (ligne de commande) depuis les
[releases](https://github.com/devfrp/ua101-rs/releases). La carte doit
être associée au pilote générique WinUSB avec Zadig : voir
[`packaging/windows/README.md`](packaging/windows/README.md).

### macOS

Télécharge `ua101-macos-universal.dmg` (Intel + Apple Silicon) et glisse
**UA-101 Panel** dans *Applications*. L'app n'est pas notariée : au premier
lancement, fais clic droit → *Ouvrir*, ou lance
`xattr -dr com.apple.quarantine "/Applications/UA-101 Panel.app"`.

## Utilisation

```sh
ua101 list                         # cartes connectées
ua101 info --raw                   # formats, endpoints, descripteur brut
ua101 monitor on --input 1         # monitoring direct de l'entrée 1
ua101 midi-in --seconds 30         # affiche le MIDI reçu
ua101 midi-out 90 3c 64            # envoie un Note On
ua101 tone --output 1 --freq 1000  # bip de test (Linux)
ua101 play fichier.wav --output 3  # lecture sur les sorties 3+ (Linux)
ua101 record prise.wav --seconds 60  # enregistre les 12 entrées (Linux)
```

La fréquence d'échantillonnage se règle avec le sélecteur de la carte. Le
pilote la lit dans les descripteurs.

## Compiler depuis les sources

```sh
cargo build --release              # tout le workspace
cargo test --workspace             # tests (aucune carte nécessaire)
cargo run -p ua101-cli -- info     # CLI
cargo run -p ua101-panel           # panneau graphique
```

Rust ≥ 1.85 (1.95 pour le panneau). Sous Linux, le panneau a besoin à la
compilation des en-têtes `libxkbcommon`, `wayland`, `libGL` et `libX11`.

## Architecture

```
crates/
  ua101-protocol   no_std, sans dépendances : descripteurs, format des
                   échantillons, taille des paquets, requête vendor, USB-MIDI
  ua101-usb        pilote : nusb (contrôle, bulk) + backend isochrone par OS
                   (Linux : usbfs), moteur full-duplex avec ring buffers
  ua101-cli        binaire `ua101`
  ua101-panel      binaire `ua101-panel` (egui)
packaging/         installeur Linux, règle udev, bundle macOS, notes Windows
docs/protocol.md   protocole USB et preuves
```

## Publier une version

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Le workflow `release.yml` compile Linux (x86_64, aarch64, musl), Windows
(`.exe`) et macOS (`.app` universelle en `.dmg`), puis publie les fichiers et
`SHA256SUMS` dans la release GitHub.

## Note légale

Ce projet est une réimplémentation indépendante, faite pour
l'interopérabilité. Il ne contient ni ne redistribue aucun fichier de
Roland : les paquets de pilotes d'origine sont exclus du dépôt. « Roland »,
« EDIROL » et « UA-101 » sont des marques de Roland Corporation, qui n'est
pas associée à ce projet.

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache-2.0](LICENSE-APACHE).
© 2026 devfrp.
