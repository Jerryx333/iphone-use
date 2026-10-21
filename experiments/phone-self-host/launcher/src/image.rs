use idevice::{
    RsdService, rsd::RsdHandshake, services::mobile_image_mounter::ImageMounter,
    tcp::handle::AdapterHandle,
};
use sha2::{Digest, Sha384};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

fn component(
    root: &Path,
    identity: &plist::Dictionary,
    key: &str,
) -> Result<PathBuf, &'static str> {
    let path = identity
        .get("Manifest")
        .and_then(|v| v.as_dictionary())
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_dictionary())
        .and_then(|v| v.get("Info"))
        .and_then(|v| v.as_dictionary())
        .and_then(|v| v.get("Path"))
        .and_then(|v| v.as_string())
        .ok_or("developer_image_manifest_invalid")?;
    if Path::new(path).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("developer_image_path_invalid");
    }
    Ok(root.join(path))
}

pub async fn mount(
    adapter: &mut AdapterHandle,
    handshake: &mut RsdHandshake,
    root: &str,
    chip: u64,
) -> Result<(), &'static str> {
    let root = Path::new(root);
    let manifest_bytes = tokio::fs::read(root.join("BuildManifest.plist"))
        .await
        .map_err(|_| "developer_image_assets_missing")?;
    let manifest: plist::Dictionary =
        plist::from_bytes(&manifest_bytes).map_err(|_| "developer_image_manifest_invalid")?;
    let mut mounter = ImageMounter::connect_rsd(adapter, handshake)
        .await
        .map_err(|_| "image_mounter_connect_failed")?;
    if !mounter
        .query_developer_mode_status()
        .await
        .map_err(|_| "developer_mode_check_failed")?
    {
        return Err("developer_mode_required");
    }
    let ids = mounter
        .query_personalization_identifiers(None)
        .await
        .map_err(|_| "image_personalization_failed")?;
    let board = ids
        .get("BoardId")
        .and_then(|v| v.as_unsigned_integer())
        .ok_or("image_personalization_failed")?;
    let ap_chip = ids
        .get("ChipID")
        .and_then(|v| v.as_unsigned_integer())
        .ok_or("image_personalization_failed")?;
    let identity = idevice::tss::select_build_identity(&manifest, board, ap_chip, None)
        .map_err(|_| "developer_image_device_unsupported")?;
    let image_path = component(root, identity, "PersonalizedDMG")?;
    let trust_path = component(root, identity, "LoadableTrustCache")?;
    let mut image = tokio::fs::File::open(&image_path)
        .await
        .map_err(|_| "developer_image_assets_missing")?;
    let image_size = image
        .metadata()
        .await
        .map_err(|_| "developer_image_read_failed")?
        .len();
    if image_size == 0 || image_size > 1_073_741_824 {
        return Err("developer_image_size_invalid");
    }
    let mut buffer = vec![0u8; 65_536];
    let mut hash = Sha384::new();
    loop {
        let count = image
            .read(&mut buffer)
            .await
            .map_err(|_| "developer_image_read_failed")?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let signature = match mounter
        .query_personalization_manifest("DeveloperDiskImage", hash.finalize().to_vec())
        .await
    {
        Ok(value) => value,
        Err(_) => {
            mounter = ImageMounter::connect_rsd(adapter, handshake)
                .await
                .map_err(|_| "image_mounter_connect_failed")?;
            mounter
                .get_manifest_from_tss(&manifest, chip)
                .await
                .map_err(|_| "apple_image_personalization_failed")?
        }
    };
    mounter
        .upload_image_file("Personalized", &image_path, signature.clone())
        .await
        .map_err(|_| "image_upload_failed")?;
    let trust = tokio::fs::read(trust_path)
        .await
        .map_err(|_| "developer_image_assets_missing")?;
    mounter
        .mount_image("Personalized", signature, Some(trust), None)
        .await
        .map_err(|_| "image_mount_failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(path: &str) -> plist::Dictionary {
        let mut info = plist::Dictionary::new();
        info.insert("Path".into(), path.into());
        let mut component = plist::Dictionary::new();
        component.insert("Info".into(), info.into());
        let mut manifest = plist::Dictionary::new();
        manifest.insert("PersonalizedDMG".into(), component.into());
        let mut identity = plist::Dictionary::new();
        identity.insert("Manifest".into(), manifest.into());
        identity
    }
    #[test]
    fn image_component_cannot_escape_bundled_assets() {
        for path in [
            "../../private.plist",
            "/private.plist",
            "image/../../../private.plist",
        ] {
            assert_eq!(
                component(
                    Path::new("/bundle/DeveloperImage"),
                    &identity(path),
                    "PersonalizedDMG"
                ),
                Err("developer_image_path_invalid")
            );
        }
        assert_eq!(
            component(
                Path::new("/bundle/DeveloperImage"),
                &identity("image.dmg"),
                "PersonalizedDMG"
            )
            .unwrap(),
            Path::new("/bundle/DeveloperImage/image.dmg")
        );
    }
}
