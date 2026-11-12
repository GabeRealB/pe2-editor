# Sound

The game's sound is made of four kinds of asset:

| Kind | What it is |
|---|---|
| **Sound** | A recorded sample. |
| **Instrument** | How a sample is played: its envelope, pitch and so on. |
| **Sound script** | What one sound effect plays: notes on instruments, over time. |
| **Sound bank** | A set of instruments and scripts, as the game loads them together. |

## Playing

Sounds, scripts and a bank's slots play in the player, as the game plays them:

| Control | What it does |
|---|---|
| **Play** / **Pause** | Plays from where it is; at the end, from the start. |
| The slider | Seeks; playback pauses while you hold it. |
| **Sound on** / **Sound off**, **Volume** | Mute and volume, kept for every project. |

A bank lists its slots in the Contents pane; choose one to play it.

**Room reverb** sets the reverb a preview plays with; by default, the one most areas use.

## The timeline

A sound script, or a bank's chosen slot, is shown as a timeline under the player: a lane of notes for each
instrument, as they play, with a playhead.

| To | Do |
|---|---|
| Seek | Click the ruler |
| Play or pause | `Space` |
| Go to the start | `Home` |
| Zoom | The mouse wheel |
| Move a note, or change its length | Drag it, or drag its end |
| Remove a note | Select it and press `Delete` |
| Add | **Add an instrument**, **Add a note** |

A selected note shows its fields to type in: **Layer**, **Delay**, **Gate**, **Pitch**, **Volume**, **Pan**,
**Reverb** and **Priority**. The player plays your changes before you save them.

## Making and exporting

- **Sound, from a WAV** makes a sound from a WAV file. It is encoded the way the console stores sound.
- **Sound script** under **File ▸ New node** makes a new, empty script.
- **File ▸ Export…** saves a sound as a WAV file, either as stored or as played.
