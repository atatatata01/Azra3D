//! Named baked-image registry (G4 / D4).
//!
//! Pixels live in FL4MATTX slots. This registry is the INDEX: name, body, kind bit,
//! res, slot, display-key. On load we reconnect to already-restored slots. A missing
//! slot is a stale badge — never a silent Live fallback.

use super::MeshMapKind;
use core_math::scene_graph::NodeId;

/// One baked map the user can pick by name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BakedImage {
    pub name: String,
    pub body: NodeId,
    pub kind_bit: u32,
    pub res: u32,
    pub slot: u32,
    pub display_key: (u64, u64),
    pub baked_at_secs: u64,
    /// Metadata present but FL4MATTX pixels did not reconnect.
    pub pixels_missing: bool,
    /// Mari UDIM (`1001` = U0 V0). Not on [`BakedImageRecord`] — `FL4UDIM` tiles carry it.
    pub udim: u32,
}

impl BakedImage {
    pub fn kind(self: &Self) -> Option<MeshMapKind> {
        MeshMapKind::from_bit(self.kind_bit)
    }

    pub fn is_stale(self: &Self, current_key: Option<(u64, u64)>) -> bool {
        self.pixels_missing || current_key.is_some_and(|k| k != self.display_key)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BakedImageRegistry {
    pub images: Vec<BakedImage>,
}

impl BakedImageRegistry {
    pub fn auto_name(body_name: &str, kind_label: &str, res: u32) -> String {
        Self::auto_name_udim(body_name, kind_label, res, super::udim::UDIM_1001, false)
    }

    pub fn auto_name_udim(
        body_name: &str,
        kind_label: &str,
        res: u32,
        udim: u32,
        multi_tile: bool,
    ) -> String {
        let stem = sanitize_stem(body_name);
        if !multi_tile && udim == super::udim::UDIM_1001 {
            format!("{stem}.{kind_label}.{res}")
        } else {
            format!("{stem}.{kind_label}.u{udim}.{res}")
        }
    }

    pub fn by_name(self: &Self, name: &str) -> Option<&BakedImage> {
        self.images.iter().find(|e| e.name == name)
    }

    pub fn by_body_kind(self: &Self, body: NodeId, kind_bit: u32) -> Option<&BakedImage> {
        self.by_body_kind_udim(body, kind_bit, super::udim::UDIM_1001)
            .or_else(|| {
                self.images
                    .iter()
                    .find(|e| e.body == body && e.kind_bit == kind_bit)
            })
    }

    pub fn by_body_kind_udim(
        self: &Self,
        body: NodeId,
        kind_bit: u32,
        udim: u32,
    ) -> Option<&BakedImage> {
        self.images
            .iter()
            .find(|e| e.body == body && e.kind_bit == kind_bit && e.udim == udim)
    }

    pub fn for_body(self: &Self, body: NodeId) -> impl Iterator<Item = &BakedImage> {
        self.images.iter().filter(move |e| e.body == body)
    }

    pub fn uniquify(self: &Self, wanted: &str) -> String {
        if self.by_name(wanted).is_none() {
            return wanted.to_string();
        }
        for i in 2..10_000 {
            let c = format!("{wanted}.{i}");
            if self.by_name(&c).is_none() {
                return c;
            }
        }
        format!("{wanted}.{}", self.images.len() + 1)
    }

    /// Insert or refresh. Same (body, kind, udim) keeps the user's name.
    pub fn upsert(self: &mut Self, mut img: BakedImage) {
        if img.udim == 0 {
            img.udim = super::udim::UDIM_1001;
        }
        if let Some(i) = self.images.iter().position(|e| {
            e.body == img.body && e.kind_bit == img.kind_bit && e.udim == img.udim
        }) {
            img.name = self.images[i].name.clone();
            self.images[i] = img;
            return;
        }
        if self.by_name(&img.name).is_some() {
            img.name = self.uniquify(&img.name);
        }
        self.images.push(img);
    }

    pub fn remove_name(self: &mut Self, name: &str) -> Option<BakedImage> {
        let i = self.images.iter().position(|e| e.name == name)?;
        Some(self.images.remove(i))
    }

    pub fn rename(self: &mut Self, old: &str, new: &str) -> Result<(), &'static str> {
        let new = new.trim();
        if new.is_empty() {
            return Err("name is empty");
        }
        if new != old && self.by_name(new).is_some() {
            return Err("that name is already used");
        }
        let Some(e) = self.images.iter_mut().find(|e| e.name == old) else {
            return Err("no such image");
        };
        e.name = new.to_string();
        Ok(())
    }

    pub fn to_records(self: &Self) -> Vec<crate::project::BakedImageRecord> {
        self.images
            .iter()
            .map(|e| crate::project::BakedImageRecord {
                name: e.name.clone(),
                node_id: e.body.0,
                kind_bit: e.kind_bit,
                res: e.res,
                slot: e.slot,
                display_key_lo: e.display_key.0,
                display_key_hi: e.display_key.1,
                baked_at_secs: e.baked_at_secs,
            })
            .collect()
    }
}

pub(crate) fn reconnect_baked_images(
    records: &[crate::project::BakedImageRecord],
    slot_has_pixels: impl Fn(u32) -> bool,
    body_exists: impl Fn(u64) -> bool,
) -> BakedImageRegistry {
    let mut reg = BakedImageRegistry::default();
    for rec in records {
        if !body_exists(rec.node_id) {
            continue;
        }
        if MeshMapKind::from_bit(rec.kind_bit).is_none() {
            continue;
        }
        reg.images.push(BakedImage {
            name: rec.name.clone(),
            body: NodeId(rec.node_id),
            kind_bit: rec.kind_bit,
            res: rec.res,
            slot: rec.slot,
            display_key: (rec.display_key_lo, rec.display_key_hi),
            baked_at_secs: rec.baked_at_secs,
            pixels_missing: rec.slot == 0 || !slot_has_pixels(rec.slot),
            udim: super::udim::UDIM_1001,
        });
    }
    reg
}

/// Write a square RGBA8 buffer as PNG. Used by the Image panel Export and the hash test.
pub(crate) fn write_rgba_png(path: &std::path::Path, rgba: &[u8]) -> Result<(), String> {
    let png = crate::material_textures::encode_map_png(rgba)
        .ok_or_else(|| "not a legal square RGBA map".to_string())?;
    std::fs::write(path, png).map_err(|e| format!("write png: {e}"))
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sanitize_stem(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() {
        "Body".into()
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str, body: u64, bit: u32) -> BakedImage {
        BakedImage {
            name: name.into(),
            body: NodeId(body),
            kind_bit: bit,
            res: 1024,
            slot: 1,
            display_key: (1, 2),
            baked_at_secs: 0,
            pixels_missing: false,
            udim: 1001,
        }
    }

    #[test]
    fn auto_name_is_body_kind_res() {
        assert_eq!(
            BakedImageRegistry::auto_name("Cube", "Curvature", 1024),
            "Cube.Curvature.1024"
        );
        assert_eq!(
            BakedImageRegistry::auto_name("My Cube!", "AO", 512),
            "My_Cube_.AO.512"
        );
    }

    #[test]
    fn upsert_two_udim_tiles_keep_both() {
        let mut r = BakedImageRegistry::default();
        let mut a = img("Cube.AO.u1001.64", 7, 2);
        a.udim = 1001;
        let mut b = img("Cube.AO.u1002.64", 7, 2);
        b.udim = 1002;
        r.upsert(a);
        r.upsert(b);
        assert_eq!(r.images.len(), 2);
        assert!(r.by_body_kind_udim(NodeId(7), 2, 1002).is_some());
        assert_eq!(r.by_body_kind(NodeId(7), 2).unwrap().udim, 1001);
    }

    #[test]
    fn upsert_same_body_kind_keeps_renamed_name() {
        let mut r = BakedImageRegistry::default();
        r.upsert(img("Cube.Curvature.1024", 7, 1));
        r.rename("Cube.Curvature.1024", "MyCurv").unwrap();
        let mut again = img("Cube.Curvature.1024", 7, 1);
        again.slot = 4;
        r.upsert(again);
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].name, "MyCurv");
        assert_eq!(r.images[0].slot, 4);
    }

    #[test]
    fn uniquify_on_name_collision_with_other_entry() {
        let mut r = BakedImageRegistry::default();
        r.upsert(img("Cube.Curvature.1024", 1, 1));
        r.upsert(img("Cube.Curvature.1024", 2, 1));
        assert_eq!(r.images[1].name, "Cube.Curvature.1024.2");
    }

    #[test]
    fn rename_rejects_empty_and_taken() {
        let mut r = BakedImageRegistry::default();
        r.upsert(img("a", 1, 1));
        r.upsert(img("b", 2, 1));
        assert!(r.rename("a", "").is_err());
        assert!(r.rename("a", "b").is_err());
        assert!(r.rename("a", "c").is_ok());
        assert!(r.by_name("c").is_some());
    }

    /// GATE (G4 D4) — pixels present + metadata present ⇒ still baked (not Live).
    #[test]
    fn reconnect_pixels_present_stays_baked() {
        let recs = [crate::project::BakedImageRecord {
            name: "Cube.Curvature.1024".into(),
            node_id: 3,
            kind_bit: MeshMapKind::Curvature.bit(),
            res: 1024,
            slot: 2,
            display_key_lo: 9,
            display_key_hi: 8,
            baked_at_secs: 1,
        }];
        let reg = reconnect_baked_images(&recs, |s| s == 2, |_| true);
        let e = reg
            .by_body_kind(NodeId(3), MeshMapKind::Curvature.bit())
            .expect("registry kept the map");
        assert!(!e.pixels_missing);
        assert_eq!(e.slot, 2);
        assert_eq!(e.name, "Cube.Curvature.1024");
    }

    /// GATE (G4 D4 negative) — metadata without pixels is stale, NOT absent.
    #[test]
    fn reconnect_missing_slot_is_stale_not_absent() {
        let recs = [crate::project::BakedImageRecord {
            name: "Cube.Curvature.1024".into(),
            node_id: 3,
            kind_bit: MeshMapKind::Curvature.bit(),
            res: 1024,
            slot: 2,
            display_key_lo: 0,
            display_key_hi: 0,
            baked_at_secs: 0,
        }];
        let reg = reconnect_baked_images(&recs, |_| false, |_| true);
        let e = reg
            .by_body_kind(NodeId(3), MeshMapKind::Curvature.bit())
            .expect("must stay registered so Auto does not go Live");
        assert!(e.pixels_missing);
        assert!(e.is_stale(None));
    }

    #[test]
    fn dangling_body_is_skipped() {
        let recs = [crate::project::BakedImageRecord {
            name: "Gone.AO.256".into(),
            node_id: 99,
            kind_bit: MeshMapKind::AmbientOcclusion.bit(),
            res: 256,
            slot: 1,
            display_key_lo: 0,
            display_key_hi: 0,
            baked_at_secs: 0,
        }];
        let reg = reconnect_baked_images(&recs, |_| true, |_| false);
        assert!(reg.images.is_empty());
    }

    /// GATE (G4 export) — encode is deterministic; decode recovers the pixels.
    #[test]
    fn export_png_hash_is_stable() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let side = 8u32;
        let mut px = vec![0u8; (side * side * 4) as usize];
        for (i, p) in px.iter_mut().enumerate() {
            *p = (i as u8).wrapping_mul(13);
        }
        let a = crate::material_textures::encode_map_png(&px).expect("png a");
        let b = crate::material_textures::encode_map_png(&px).expect("png b");
        let hash = |v: &[u8]| {
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&a), hash(&b), "export must be deterministic");
        let back = crate::material_textures::decode_map_png(&a).expect("decode");
        assert_eq!(back, px);
        let dir = std::env::temp_dir().join(format!("azra_bake_png_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("e.png");
        write_rgba_png(&path, &px).expect("write");
        let on_disk = std::fs::read(&path).expect("read");
        assert_eq!(hash(&on_disk), hash(&a));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
