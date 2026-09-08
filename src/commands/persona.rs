//! Persona command handler.

use anyhow::Result;

use crate::cli::PersonaCommands;
use crate::persona::shared::{
    load_current_persona, reset_persona, save_persona_to_file, set_behavior, set_length, set_name,
    set_no_em_dashes, set_no_emojis, set_persona_file, set_tone,
};

pub fn handle_persona(action: PersonaCommands) -> Result<()> {
    match action {
        PersonaCommands::Show => {
            let persona = load_current_persona()?;
            println!("Name: {}", persona.name);
            println!("Behavior:\n{}", persona.behavior);
            println!();
            println!("Style:");
            println!("  Length: {}", persona.style.length);
            println!("  Tone: {}", persona.style.tone);
            println!("  No em dashes: {}", persona.style.formatting.no_em_dashes);
            println!("  No emojis: {}", persona.style.formatting.no_emojis);
        }
        PersonaCommands::Edit => {
            println!("Use subcommands like `ahnara persona name ...`, `behavior ...`, `style ...`, `load ...`, or `reset`.");
        }
        PersonaCommands::Name { name } => {
            set_name(&name)?;
            println!("Persona name updated: {}", name);
        }
        PersonaCommands::Behavior { text } => {
            set_behavior(&text)?;
            println!("Persona behavior updated");
        }
        PersonaCommands::Style {
            length,
            tone,
            no_em_dashes,
            no_emojis,
        } => {
            if let Some(length) = length {
                set_length(&length)?;
                println!("Persona length updated: {}", length);
            }
            if let Some(tone) = tone {
                set_tone(&tone)?;
                println!("Persona tone updated: {}", tone);
            }
            if no_em_dashes {
                set_no_em_dashes(true)?;
                println!("Persona no_em_dashes enabled");
            }
            if no_emojis {
                set_no_emojis(true)?;
                println!("Persona no_emojis enabled");
            }
        }
        PersonaCommands::Load { file } => {
            set_persona_file(&file)?;
            println!("Persona file set: {}", file);
        }
        PersonaCommands::Save { output } => {
            let path = save_persona_to_file(output.as_deref())?;
            println!("Saved current persona to {}", path.display());
        }
        PersonaCommands::Reset => {
            reset_persona()?;
            println!("Persona reset to default");
        }
    }
    Ok(())
}
