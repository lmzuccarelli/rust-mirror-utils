use custom_logger::*;
use mirror_catalog_index::ManifestSchema;
use mirror_copy::{FsLayer, ImageReference, Manifest, ManifestList};
use mirror_error::MirrorError;
use serde_derive::{Deserialize, Serialize};
use sha256::digest;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialOrd, PartialEq, Ord, Eq)]
pub struct MirrorImageInfo {
    #[serde(rename = "indexReference")]
    pub reference: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "namespace")]
    pub namespace: String,
    #[serde(rename = "digest")]
    pub digest: String,
    #[serde(rename = "tag")]
    pub tag: Option<String>,
    #[serde(rename = "arch")]
    pub arch: String,
    #[serde(rename = "manifestType")]
    pub manifest_type: String,
    #[serde(rename = "creationTimestamp")]
    pub created: String,
    #[serde(rename = "mirrorType")]
    pub mirror_type: String,
    #[serde(rename = "bundle")]
    pub bundle: Option<String>,
}

// used to drive a spinner
#[allow(unused)]
pub mod keepalive {
    use std::sync::{Arc, Weak};

    pub struct Sender(Arc<()>);

    #[derive(Clone)]
    pub struct Receiver(Weak<()>);

    pub fn channel() -> (Sender, Receiver) {
        let arc = Arc::new(());
        let weak = Arc::downgrade(&arc);
        (Sender(arc), Receiver(weak))
    }

    impl Receiver {
        pub fn is_alive(&self) -> bool {
            Weak::strong_count(&self.0) > 0
        }
    }
}

pub fn parse_image(log: &Logging, image: String) -> ImageReference {
    // check if we have digest
    log.debug(&format!("[parse_image] parsing image {}", image.clone()));
    if image.contains("@") {
        let mut hld = image.split("@");
        let img = hld.nth(0).unwrap();
        // strip registry out
        let digest = hld.nth(0).unwrap();
        let vec_comp = img.split("/").collect::<Vec<&str>>();
        if vec_comp.len() == 3 {
            let ir = ImageReference {
                registry: vec_comp[0].to_string(),
                namespace: vec_comp[1].to_string(),
                name: vec_comp[2].to_string(),
                version: digest.to_string(),
            };
            log.debug(&format!("image input {}", image.clone()));
            log.debug(&format!("image digest reference {:#?}", ir.clone()));
            return ir;
        } else {
            let ir = ImageReference {
                registry: "".to_string(),
                namespace: "".to_string(),
                name: "".to_string(),
                version: "".to_string(),
            };
            return ir;
        }
    } else {
        let empty_ir = ImageReference {
            registry: "".to_string(),
            namespace: "".to_string(),
            name: "".to_string(),
            version: "".to_string(),
        };
        let components = image.split("/").collect::<Vec<&str>>();
        if components.len() >= 3 {
            if components[2].to_string().contains(":") {
                let name = components[2].split(":").nth(0).unwrap();
                let tag = components[2].split(":").nth(1).unwrap();
                let ir = ImageReference {
                    registry: components[0].to_string(),
                    namespace: components[1].to_string(),
                    name: name.to_string(),
                    version: tag.to_string(),
                };
                log.debug(&format!("image input {}", image.clone()));
                log.debug(&format!("image tag reference {:#?}", ir.clone()));
                return ir;
            } else {
                return empty_ir;
            }
        } else {
            return empty_ir;
        }
    }
}

pub async fn process_fb_image(
    dir: String,
    oci: String,
    reference: String,
    tag_digest: String,
    mirror_type: String,
) -> Result<MirrorImageInfo, MirrorError> {
    let index_json = format!("{}/manifest.json", &oci);
    let mnfst = read_and_parse_oci_manifest(index_json.clone())?;
    for mn in mnfst.layers.unwrap().iter() {
        let digest = mn.digest.replace(":", "/");
        let blob = digest.split("sha256/").nth(1).unwrap();
        let to_path = format!("{}/blobs-store/{}", dir.clone(), &blob[0..2]);
        fs_handler(to_path.clone(), "create_dir", None).await?;
        fs_copy(
            format!("{}/{}", &oci, blob),
            format!("{}/{}", to_path, blob),
        )
        .await?
    }
    // copy the config
    let cfg = mnfst.config;
    let blob = cfg.as_ref().unwrap().digest.split(":").nth(1).unwrap();
    let to = format!("{}/blobs-store/{}/{}", dir.clone(), &blob[0..2], blob);
    let to_path = format!("{}/blobs-store/{}", dir.clone(), &blob[0..2]);
    fs_handler(to_path.clone(), "create_dir", None).await?;
    fs_copy(format!("{}/{}", &oci, blob), to).await?;
    // finally write the manifest
    let manifest_file = format!(
        "{}/manifests/{}/{}:{}-amd64.json",
        dir.clone(),
        &mirror_type,
        oci,
        tag_digest
    );
    let data = fs_handler(index_json, "read", None).await?;
    // before writing do some sneaky updates
    let updated_data = data.replace(
        "application/vnd.docker.image.rootfs.diff.tar",
        "application/vnd.docker.image.rootfs.diff.tar.gzip",
    );
    fs_handler(manifest_file, "write", Some(updated_data)).await?;

    let mut mii = MirrorImageInfo {
        // TODO:should fix this to parse image
        reference: oci.to_string(),
        name: oci.to_string(),
        arch: "amd64".to_string(),
        namespace: reference + &"/" + &oci,
        digest: "".to_string(),
        manifest_type: "manifest".to_string(),
        tag: Some(tag_digest.clone()),
        created: "".to_string(),
        mirror_type: mirror_type.to_string(),
        bundle: None,
    };

    if tag_digest.contains("sha256:") {
        mii.digest = blob.to_string();
        mii.tag = None;
    }
    Ok(mii)
}

pub fn remove_duplicates(
    dir: String,
    map_in: HashMap<String, Vec<FsLayer>>,
) -> HashMap<String, Vec<FsLayer>> {
    let mut map: HashMap<String, Vec<FsLayer>> = HashMap::new();
    let mut vec_layer: Vec<FsLayer> = Vec::new();
    // remove duplicates
    for (k, v) in map_in.iter() {
        for layer in v.iter() {
            // scrub out duplicates
            let truncated_image = layer.blob_sum.split(":").nth(1).unwrap();
            let inner_blobs_file = format!(
                "{}/{}/{}/{}",
                dir.clone(),
                "blobs-store",
                &truncated_image[0..2],
                truncated_image
            );
            let mut exists = Path::new(&inner_blobs_file).exists();
            if exists {
                let metadata = fs::metadata(&inner_blobs_file).unwrap();
                if layer.size.is_some() {
                    if metadata.len() != layer.size.unwrap() as u64 {
                        exists = false;
                    }
                } else {
                    exists = false;
                }
            }
            if !exists {
                vec_layer.insert(0, layer.clone());
            }
        }
        map.insert(k.to_string(), vec_layer.clone());
    }
    map
}

pub async fn fs_open_or_create(name: String, create: bool) -> Result<File, MirrorError> {
    let res: Result<std::fs::File, std::io::Error>;
    if create {
        res = std::fs::File::create(&name);
    } else {
        res = std::fs::File::open(&name);
    }
    if res.is_err() {
        return Err(MirrorError::new(&format!(
            "[fs_open] {}",
            res.as_ref().err().unwrap().to_string().to_lowercase(),
        )));
    }
    Ok(res.unwrap())
}

pub async fn fs_copy(from: String, to: String) -> Result<(), MirrorError> {
    let res = fs::copy(from.clone(), to.clone());
    if res.is_err() {
        return Err(MirrorError::new(&format!(
            "[fs_copy] from {} : to {} {}",
            from,
            to,
            res.as_ref().err().unwrap().to_string().to_lowercase(),
        )));
    }
    Ok(())
}

pub async fn fs_handler(
    dir_file: String,
    mode: &str,
    data: Option<String>,
) -> Result<String, MirrorError> {
    match mode {
        "create_dir" => {
            let res = fs::create_dir_all(&dir_file);
            if res.is_err() {
                let err = MirrorError::new(&format!(
                    "[fs_handler] creating directory {} {}",
                    dir_file,
                    res.err().unwrap().to_string().to_lowercase()
                ));
                return Err(err);
            }
        }
        "remove_file" => {
            let res = fs::remove_file(&dir_file);
            if res.is_err() {
                let err = MirrorError::new(&format!(
                    "[fs_handler] deleting file {} {}",
                    dir_file,
                    res.err().unwrap().to_string().to_lowercase()
                ));
                return Err(err);
            }
        }
        "remove_dir" => {
            let res = fs::remove_dir_all(&dir_file);
            if res.is_err() {
                let err = MirrorError::new(&format!(
                    "[fs_handler] deleting directory {} {}",
                    dir_file,
                    res.err().unwrap().to_string().to_lowercase()
                ));
                return Err(err);
            }
        }
        "write" => {
            let res = fs::write(&dir_file, data.unwrap());
            if res.is_err() {
                let err = MirrorError::new(&format!(
                    "[fs_handler] writing file {} {}",
                    dir_file,
                    res.err().unwrap().to_string().to_lowercase()
                ));
                return Err(err);
            }
        }
        "read" => {
            let res = fs::read_to_string(&dir_file);
            if res.is_err() {
                let err = MirrorError::new(&format!(
                    "[fs_handler] reading file {} {}",
                    dir_file,
                    res.err().unwrap().to_string().to_lowercase()
                ));
                return Err(err);
            }
            return Ok(res.unwrap());
        }
        _ => {
            let err = MirrorError::new(&format!("[fs_handler] mode {} not supported", dir_file,));
            return Err(err);
        }
    }
    Ok("ok".to_string())
}

// verify_file - function to check size and sha256 hash of contents
pub async fn verify_file(
    _log: &Logging,
    dir: String,
    blob_sum: String,
    blob_size: u64,
    data: Vec<u8>,
) -> Result<(), MirrorError> {
    let f = &format!("{}/{}", dir, blob_sum);
    let res = fs::metadata(&f);
    if res.is_ok() {
        if res.unwrap().size() != blob_size {
            let err = MirrorError::new(&format!(
                "sha256 file size don't match {}",
                blob_size.clone()
            ));
            return Err(err);
        }
        let hash = digest(&data);
        if hash != blob_sum {
            let err = MirrorError::new(&format!(
                "sha256 hash contents don't match {} {}",
                hash,
                blob_sum.clone()
            ));
            return Err(err);
        }
    } else {
        let err = MirrorError::new(&format!("sha256 hash metadata file {}", f));
        return Err(err);
    }
    Ok(())
}

// parse the manifest json
pub fn parse_json_metadata(data: String) -> Result<Vec<MirrorImageInfo>, MirrorError> {
    // Parse the string of data into serde_json::Manifest.
    let res = serde_json::from_str(&data);
    if res.is_err() {
        return Err(MirrorError::new(&format!(
            "[parse_json_metadata] {}",
            res.err().unwrap().to_string().to_lowercase()
        )));
    }
    let root: Vec<MirrorImageInfo> = res.unwrap();
    Ok(root)
}

// parse the manifest json
pub fn parse_json_manifest(data: String) -> Result<ManifestSchema, MirrorError> {
    // Parse the string of data into serde_json::ManifestSchema.
    let res = serde_json::from_str(&data);
    if res.is_err() {
        let err = MirrorError::new(&format!(
            "[parse_json_manifest] {}",
            res.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let root: ManifestSchema = res.unwrap();
    Ok(root)
}

// parse the manifestlist json
pub fn parse_json_manifestlist(data: String) -> Result<ManifestList, MirrorError> {
    // Parse the string of data into serde_json::ManifestList.
    let res = serde_json::from_str(&data);
    if res.is_err() {
        let err = MirrorError::new(&format!(
            "[parse_json_manifestlist] manifestlist {}",
            res.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let root: ManifestList = res.unwrap();
    Ok(root)
}

// parse the oci manifest json
pub fn parse_json_manifest_operator(data: String) -> Result<Manifest, MirrorError> {
    // Parse the string of data into serde_json::Manifest.
    let res = serde_json::from_str(&data);
    if res.is_err() {
        let err = MirrorError::new(&format!(
            "[parse_json_manifest_operator] {}",
            res.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let root: Manifest = res.unwrap();
    Ok(root)
}

pub fn read_and_parse_manifest(file: String) -> Result<ManifestSchema, MirrorError> {
    let data = fs::read_to_string(file.clone());
    if data.is_err() {
        let err = MirrorError::new(&format!(
            "[read_and_parse_manifest] reading manifest data {} {}",
            file,
            data.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let manifest = parse_json_manifest(data.unwrap())?;
    Ok(manifest.clone())
}

pub fn read_and_parse_oci_manifest(file: String) -> Result<Manifest, MirrorError> {
    let data = fs::read_to_string(file.clone());
    if data.is_err() {
        let err = MirrorError::new(&format!(
            "[read_and_parse_oci_manifest] reading oci manifest data {} {}",
            file,
            data.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let manifest = parse_json_manifest_operator(data.unwrap())?;
    Ok(manifest.clone())
}

pub fn read_and_parse_oci_manifestlist(file: String) -> Result<ManifestList, MirrorError> {
    let data = fs::read_to_string(file.clone());
    if data.is_err() {
        let err = MirrorError::new(&format!(
            "[read_and_parse_oci_manifestlist] reading oci manifest data {} {}",
            file,
            data.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let manifest = parse_json_manifestlist(data.unwrap())?;
    Ok(manifest.clone())
}

pub fn read_and_parse_metadata(file: String) -> Result<Vec<MirrorImageInfo>, MirrorError> {
    let data = fs::read_to_string(file.clone());
    if data.is_err() {
        let err = MirrorError::new(&format!(
            "[read_and_parse_oci_manifestlist] reading mirror-metadata {} {}",
            file,
            data.err().unwrap().to_string().to_lowercase(),
        ));
        return Err(err);
    }
    let md = parse_json_metadata(data.unwrap())?;
    Ok(md.clone())
}

pub async fn process_and_update_manifest(
    log: &Logging,
    manifest: String,
    file: String,
    file_override: HashMap<String, String>,
) -> Result<Option<String>, MirrorError> {
    log.debug(&format!(
        "[pocess_and_update_manifest] file {} ",
        file.clone()
    ));
    let mut changed = false;
    let res_file = file_override.get(&file.clone());
    if res_file.is_some() {
        log.debug(&format!(
            "[process_and_update_manifest] using override file {}",
            res_file.unwrap()
        ));
        return Ok(Some(res_file.unwrap().to_string()));
    }
    let exists = Path::new(&file.clone()).exists();
    if exists {
        let manifest_on_disk = fs::read_to_string(file.clone());
        if manifest_on_disk.is_ok() {
            if manifest_on_disk.unwrap() != manifest {
                changed = true;
            }
        } else {
            changed = true;
        }
    }
    if !exists || changed {
        fs_handler(file.clone(), "write", Some(manifest.clone().to_string())).await?;
        return Ok(Some(file.clone()));
    }
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! aw {
        ($e:expr) => {
            tokio_test::block_on($e)
        };
    }

    #[test]
    fn fs_handler_all_fail() {
        let log = &Logging {
            log_level: Level::INFO,
        };

        let res = aw!(fs_handler("/root/crap".to_string(), "create_dir", None));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);

        let res = aw!(fs_handler("/root/crap".to_string(), "remove_dir", None));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);

        let res = aw!(fs_handler(
            "/root/crap.txt".to_string(),
            "remove_file",
            None
        ));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);

        let res = aw!(fs_handler("/root/crap.txt".to_string(), "read", None));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);

        let res = aw!(fs_handler(
            "/root/crap.txt".to_string(),
            "write",
            Some("nonesense".to_string())
        ));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn fs_verify_file_pass() {
        let log = &Logging {
            log_level: Level::INFO,
        };

        macro_rules! aw {
            ($e:expr) => {
                tokio_test::block_on($e)
            };
        }

        let data = fs::read_to_string(
            "test-artifacts/c9e9e89d3e43c791365ec19dc5acd1517249a79c09eb482600024cd1c6475abe",
        )
        .expect("should read file");

        let res = aw!(verify_file(
            log,
            "test-artifacts".to_string(),
            "c9e9e89d3e43c791365ec19dc5acd1517249a79c09eb482600024cd1c6475abe".to_string(),
            504,
            data.into_bytes()
        ));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_ok(), true);
    }
    #[test]
    fn fs_verify_file_fail() {
        let log = &Logging {
            log_level: Level::INFO,
        };

        macro_rules! aw {
            ($e:expr) => {
                tokio_test::block_on($e)
            };
        }

        let data = fs::read_to_string(
            "test-artifacts/c9e9e89d3e43c791365ec19dc5acd1517249a79c09eb482600024cd1c6475abe",
        )
        .expect("should read file");

        let res = aw!(verify_file(
            log,
            "test-artifacts".to_string(),
            "c9e9e89d3e43c791365ec19dc5acd1517249a79c09eb482600024cd1c6475abe".to_string(),
            100,
            data.clone().into_bytes()
        ));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);

        let res = aw!(verify_file(
            log,
            "test-artifacts".to_string(),
            "sha256:65e311ef7036acc3692d291403656b840fd216d120b3c37af768f91df050257d".to_string(),
            428,
            data.clone().into_bytes()
        ));
        if res.is_err() {
            log.error(&format!(
                "result -> {}",
                res.as_ref().err().unwrap().to_string().to_lowercase()
            ));
        }
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn parse_json_manifest_fail() {
        let data = "{ ".to_string();
        let res = parse_json_manifest(data);
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn parse_json_manifestlist_fail() {
        let data = "{ ".to_string();
        let res = parse_json_manifestlist(data);
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn parse_json_manifest_operator_fail() {
        let data = "{ ".to_string();
        let res = parse_json_manifest_operator(data);
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn read_and_parse_manifest_fail() {
        let res = read_and_parse_manifest(".nofile".to_string());
        assert_eq!(res.is_err(), true);
        let res = read_and_parse_manifest("test-artifacts/bad.json".to_string());
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn read_and_parse_oci_manifestlist_fail() {
        let res = read_and_parse_oci_manifestlist(".nofile".to_string());
        assert_eq!(res.is_err(), true);
        let res = read_and_parse_oci_manifestlist("test-artifacts/bad.json".to_string());
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn read_and_parse_oci_manifest_fail() {
        let res = read_and_parse_oci_manifest(".nofile".to_string());
        assert_eq!(res.is_err(), true);
        let res = read_and_parse_manifest("test-artifacts/bad.json".to_string());
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn parse_image_tag_pass() {
        let log = &Logging {
            log_level: Level::INFO,
        };
        let res = parse_image(
            log,
            "registry.redhat.io/redhat/redhat-operator-index:v4.15".to_string(),
        );
        assert_eq!(res.registry, "registry.redhat.io");
        assert_eq!(res.namespace, "redhat");
        assert_eq!(res.name, "redhat-operator-index");
        assert_eq!(res.version, "v4.15");
    }
    #[test]
    fn parse_image_digest_pass() {
        let log = &Logging {
            log_level: Level::INFO,
        };
        let res = parse_image(
            log,
            "quay.io/ocp-release/ocp-release-dev@sha256:abcdef321323213123123123131".to_string(),
        );
        assert_eq!(res.registry, "quay.io");
        assert_eq!(res.namespace, "ocp-release");
        assert_eq!(res.name, "ocp-release-dev");
        assert_eq!(res.version, "sha256:abcdef321323213123123123131");
    }
    #[test]
    fn parse_image_bad_image_pass() {
        let log = &Logging {
            log_level: Level::INFO,
        };
        let res = parse_image(
            log,
            "quay.io/ocp-release-dev@sha256:abcdef321323213123123123131".to_string(),
        );
        assert_eq!(res.registry, "");
        assert_eq!(res.namespace, "");
        assert_eq!(res.name, "");
        assert_eq!(res.version, "");
    }
    #[test]
    fn remove_duplicates_pass() {
        let mut map_in: HashMap<String, Vec<FsLayer>> = HashMap::new();
        let mut vec_layer: Vec<FsLayer> = Vec::new();
        let fs_a = FsLayer {
            blob_sum: "sha256:abcdef321323213123123123131".to_string(),
            size: Some(10),
            original_ref: Some("test.io/test/test-index:v1.23".to_string()),
            number: None,
        };
        vec_layer.insert(0, fs_a);
        let fs_b = FsLayer {
            blob_sum: "sha256:abcdef321323213123123123131".to_string(),
            size: Some(10),
            original_ref: Some("test.io/test/test-index:v1.23".to_string()),
            number: None,
        };
        vec_layer.insert(0, fs_b);
        let fs_c = FsLayer {
            blob_sum: "sha256:efefacd98765432101234567890".to_string(),
            size: Some(200),
            original_ref: Some("test.io/test/test-index:v1.23".to_string()),
            number: None,
        };
        vec_layer.insert(0, fs_c);
        map_in.insert("test.io/test/test-index".to_string(), vec_layer.clone());
        let res = remove_duplicates("test-artifacts".to_string(), map_in.clone());
        let vec_out = res.get("test.io/test/test-index").unwrap();
        assert_eq!(vec_out.len(), 1)
    }
    #[test]
    fn fs_open_or_create_pass() {
        let res = aw!(fs_open_or_create("test".to_string(), true));
        assert_eq!(res.is_ok(), true);
        let res = aw!(fs_open_or_create(
            "test-artifacts/c9e9e89d3e43c791365ec19dc5acd1517249a79c09eb482600024cd1c6475abe"
                .to_string(),
            false
        ));
        assert_eq!(res.is_ok(), true);
    }
    #[test]
    fn fs_open_or_create_fail() {
        let res = aw!(fs_open_or_create("/root/test".to_string(), true));
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn fs_copy_fail() {
        let res = aw!(fs_copy("/root/test".to_string(), "/root/nada".to_string()));
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn parse_json_metadata_pass() {
        let data = aw!(fs_handler(
            "test-artifacts/additional-image-reference.json".to_string(),
            "read",
            None
        ));
        let res = parse_json_metadata(data.unwrap().to_string());
        assert_eq!(res.is_ok(), true);
    }
    #[test]
    fn parse_json_metadata_fail() {
        let res = parse_json_metadata("{ ".to_string());
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn read_and_parse_json_metadata_pass() {
        let res =
            read_and_parse_metadata("test-artifacts/additional-image-reference.json".to_string());
        assert_eq!(res.is_ok(), true);
    }
    #[test]
    fn read_and_parse_json_metadata_fail() {
        let res = read_and_parse_metadata("nada".to_string());
        assert_eq!(res.is_err(), true);
    }
    #[test]
    fn process_and_update_manifest_pass() {
        let log = &Logging {
            log_level: Level::INFO,
        };
        let data = aw!(fs_handler(
            "test-artifacts/manifest.json".to_string(),
            "read",
            None
        ));
        let map: HashMap<String, String> = HashMap::new();
        let res = aw!(process_and_update_manifest(
            log,
            data.unwrap(),
            "test-artifacts/manifest.json".to_string(),
            map
        ));
        assert_eq!(res.is_ok(), true);
        assert_eq!(res.unwrap().is_none(), true);
    }
    #[test]
    fn process_and_update_manifest_override_pass() {
        let log = &Logging {
            log_level: Level::DEBUG,
        };
        let data = aw!(fs_handler(
            "test-artifacts/manifest.json".to_string(),
            "read",
            None
        ));
        let mut map: HashMap<String, String> = HashMap::new();
        map.insert(
            "test-artifacts/manifest.json".to_string(),
            "test.json".to_string(),
        );
        let res = aw!(process_and_update_manifest(
            log,
            data.unwrap(),
            "test-artifacts/manifest.json".to_string(),
            map
        ));
        assert_eq!(res.is_ok(), true);
        assert_eq!(res.as_ref().unwrap().is_some(), true);
        assert_eq!(res.unwrap().unwrap(), "test.json");
    }
    #[test]
    fn process_fb_image_pass() {
        let res = aw!(process_fb_image(
            "test-artifacts".to_string(),
            "test-artifacts/test-oci".to_string(),
            "test-index_image".to_string(),
            "v1.0".to_string(),
            "operator".to_string()
        ));
        assert_eq!(res.is_ok(), true)
    }
}
