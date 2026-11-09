//! Celestial warp-in points, ported from the client (eve/common/script/sys/eveCfg.py
//! Get*WarpInPoint, ballparkCommon.parkhelpers.HasWarpInPoint). The client marks
//! these spots itself (michelle.GetWarpinPoints), so they must match exactly.

use super::V3;
use infinity_core::consts::GROUP_WRECK;

const CATEGORY_CELESTIAL: i32 = 2;
const GROUP_SUN: i32 = 6;
const GROUP_PLANET: i32 = 7;
/// appConst.jumpRadiusFactor.
const JUMP_RADIUS_FACTOR: f64 = 130.0;

/// HasWarpInPoint: big celestials (sun, planets, moons, belts) land you at a fixed
/// spot instead of off their surface.
pub fn warp_in_point(id: i64, category_id: i32, group_id: i32, pos: V3, radius: f64) -> Option<V3> {
    if radius < 90000.0 || category_id != CATEGORY_CELESTIAL || group_id == GROUP_WRECK {
        return None;
    }
    Some(match group_id {
        GROUP_PLANET => planet(id, pos, radius)?,
        GROUP_SUN => {
            let d = radius + 100000.0;
            [pos[0] + d * radius.cos(), pos[1] + radius / 5.0, pos[2] - d * radius.sin()]
        }
        _ => {
            let d = radius + 5000000.0;
            [pos[0] + d * radius.cos(), pos[1] + JUMP_RADIUS_FACTOR / 100.0 * radius - 7500.0, pos[2] - d * radius.sin()]
        }
    })
}

/// GetPlanetWarpInPoint.
fn planet(id: i64, pos: V3, radius: f64) -> Option<V3> {
    let (dx, dz) = (pos[0], -pos[2]);
    let h = (dx * dx + dz * dz).sqrt();
    if h == 0.0 {
        return None;
    }
    let mut f = dz / h;
    if dx > 0.0 && dz != 0.0 {
        f = -f;
    }
    let rr = (PyRandom::new(id.unsigned_abs()).random() - 1.0) / 3.0;
    let theta = f.asin() + rr;
    const FACTOR: f64 = 20.0;
    let dd = (((FACTOR - 5.0 * (radius / 1000000.0).log10() - 0.5) / FACTOR).powf(FACTOR) * FACTOR).clamp(0.0, 10.0) + 0.5;
    let d = radius + 1000000.0 + radius * dd;
    Some([pos[0] + theta.sin() * d, pos[1] + radius * rr.sin() * 0.5, pos[2] - theta.cos() * d])
}

/// Python 2.7 `random.Random(int)`: MT19937 seeded by init_by_array over the
/// seed's 32-bit words, `random()` built from two draws.
struct PyRandom {
    mt: [u32; 624],
    index: usize,
}

impl PyRandom {
    fn new(seed: u64) -> Self {
        let mut r = PyRandom { mt: [0; 624], index: 624 };
        r.mt[0] = 19650218;
        for i in 1..624 {
            r.mt[i] = 1812433253u32.wrapping_mul(r.mt[i - 1] ^ (r.mt[i - 1] >> 30)).wrapping_add(i as u32);
        }
        let key: Vec<u32> = if seed >> 32 == 0 { vec![seed as u32] } else { vec![seed as u32, (seed >> 32) as u32] };
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..624.max(key.len()) {
            r.mt[i] = (r.mt[i] ^ (r.mt[i - 1] ^ (r.mt[i - 1] >> 30)).wrapping_mul(1664525)).wrapping_add(key[j]).wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 { r.mt[0] = r.mt[623]; i = 1; }
            if j >= key.len() { j = 0; }
        }
        for _ in 0..623 {
            r.mt[i] = (r.mt[i] ^ (r.mt[i - 1] ^ (r.mt[i - 1] >> 30)).wrapping_mul(1566083941)).wrapping_sub(i as u32);
            i += 1;
            if i >= 624 { r.mt[0] = r.mt[623]; i = 1; }
        }
        r.mt[0] = 0x8000_0000;
        r
    }

    fn next_u32(&mut self) -> u32 {
        if self.index >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                self.mt[k] = self.mt[(k + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908_b0df } else { 0 };
            }
            self.index = 0;
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    fn random(&mut self) -> f64 {
        let a = (self.next_u32() >> 5) as f64;
        let b = (self.next_u32() >> 6) as f64;
        (a * 67108864.0 + b) / 9007199254740992.0
    }
}

#[cfg(test)] mod tests;
