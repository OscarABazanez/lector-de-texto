//! Guarda la API key de Anthropic en el almacén de credenciales, igual que «Guardar key» en Ajustes.
//!
//!   cargo run --example api_key            (toma la key de ANTHROPIC_API_KEY)
//!   cargo run --example api_key -- --clear (la borra)

use lector_lib::secrets;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--clear") {
        secrets::clear_api_key()?;
        println!("API key borrada del almacén de credenciales.");
        return Ok(());
    }
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("define ANTHROPIC_API_KEY con la key a guardar"))?;
    secrets::set_api_key(&key)?;
    match secrets::get_api_key() {
        Some((k, source)) => println!("Guardada ({source:?}): …{}", &k[k.len().saturating_sub(4)..]),
        None => anyhow::bail!("no se pudo leer la key recién guardada"),
    }
    Ok(())
}
