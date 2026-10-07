//! Bounded GNOME XML slideshow parsing and wall-clock sampling.
//! Parsing and local-calendar conversion belong on wallpaper workers.
use std::path::PathBuf;

pub const MAX_XML_BYTES: usize = 1024 * 1024;
const MAX_SLIDES: usize = 512;
const MAX_VARIANTS: usize = 64;

#[derive(Debug, Clone)]
pub struct Variant {
    pub dimensions: [i32; 2],
    pub path: PathBuf,
}
#[derive(Debug, Clone)]
pub struct Slide {
    pub duration: f64,
    pub from: Vec<Variant>,
    pub to: Vec<Variant>,
}
#[derive(Debug, Clone)]
pub struct Timeline {
    pub start: f64,
    pub total: f64,
    pub slides: Vec<Slide>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub wall: f64,
    pub slide: usize,
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub progress: f64,
    pub interval: f64,
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    let mut found = node.children().filter(|n| n.has_tag_name(name));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}
fn variants(node: roxmltree::Node<'_, '_>) -> Option<Vec<Variant>> {
    let path = |text: &str| {
        (!text.is_empty() && text.len() <= 4096 && !text.contains('\0'))
            .then(|| PathBuf::from(text))
    };
    let mut result = Vec::new();
    for n in node.children() {
        if n.has_tag_name("size") {
            let width: i32 = n.attribute("width")?.parse().ok()?;
            let height: i32 = n.attribute("height")?.parse().ok()?;
            if width <= 0 || height <= 0 {
                return None;
            }
            result.push(Variant {
                dimensions: [width, height],
                path: path(n.text()?)?,
            });
        } else if n.is_text() && n.text().is_some_and(|t| !t.trim().is_empty()) {
            result.push(Variant {
                dimensions: [-1, -1],
                path: path(n.text()?)?,
            });
        }
        if result.len() > MAX_VARIANTS {
            return None;
        }
    }
    // GnomeBGSlideShow prepends variants; equal-distance ties retain that order.
    result.reverse();
    (!result.is_empty()).then_some(result)
}
impl Timeline {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_XML_BYTES {
            return None;
        }
        let xml = std::str::from_utf8(bytes).ok()?;
        let doc = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 4096,
            },
        )
        .ok()?;
        let root = doc.root_element();
        if !root.has_tag_name("background") {
            return None;
        }
        // Missing calendar components retain GNOME's localtime(epoch) defaults.
        let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
        let epoch: libc::time_t = 0;
        // SAFETY: both pointers are valid and localtime_r initializes tm on success.
        if unsafe { libc::localtime_r(&epoch, calendar.as_mut_ptr()) }.is_null() {
            return None;
        }
        // SAFETY: successful localtime_r initialized every tm field.
        let mut calendar = unsafe { calendar.assume_init() };
        if root
            .children()
            .filter(|n| n.has_tag_name("starttime"))
            .count()
            > 1
        {
            return None;
        }
        if let Some(start) = child(root, "starttime") {
            for n in start.children().filter(|n| n.is_element()) {
                let text = n.text().unwrap_or_default().trim();
                match n.tag_name().name() {
                    "year" => calendar.tm_year = text.parse::<i32>().ok()?.checked_sub(1900)?,
                    "month" => calendar.tm_mon = text.parse::<i32>().ok()?.checked_sub(1)?,
                    "day" => calendar.tm_mday = text.parse().ok()?,
                    "hour" => calendar.tm_hour = text.parse().ok()?,
                    "minute" => calendar.tm_min = text.parse().ok()?,
                    "second" => calendar.tm_sec = text.parse().ok()?,
                    _ => {} // Harmless extension elements do not invalidate GNOME XML.
                }
            }
        }
        calendar.tm_isdst = -1;
        // SAFETY: initialized local tm; mktime supplies GNOME calendar normalization/DST.
        let start = unsafe { libc::mktime(&mut calendar) } as f64;
        let mut slides = Vec::new();
        let mut total = 0.0;
        for n in root.children().filter(|n| n.is_element()) {
            let fixed = match n.tag_name().name() {
                "static" => true,
                "transition" => false,
                _ => continue,
            };
            let duration: f64 = child(n, "duration")?.text()?.trim().parse().ok()?;
            if !duration.is_finite() || duration <= 0.0 {
                return None;
            }
            let from = variants(child(n, if fixed { "file" } else { "from" })?)?;
            let to = if fixed {
                Vec::new()
            } else {
                variants(child(n, "to")?)?
            };
            slides.push(Slide { duration, from, to });
            total += duration;
            if slides.len() > MAX_SLIDES || !total.is_finite() {
                return None;
            }
        }
        if slides.is_empty() {
            return None;
        }
        if slides.len() == 1 {
            total = f64::from(u32::MAX);
            slides[0].duration = total;
        }
        Some(Self {
            start,
            total,
            slides,
        })
    }
    pub fn sample(&self, now: f64, dimensions: [i32; 2]) -> Option<Sample> {
        if !now.is_finite() || dimensions.iter().any(|v| *v <= 0) {
            return None;
        }
        let delta = (now - self.start).rem_euclid(self.total);
        let mut elapsed = 0.0;
        for (index, slide) in self.slides.iter().enumerate() {
            if elapsed + slide.duration > delta {
                return Some(Sample {
                    wall: now,
                    slide: index,
                    from: best(&slide.from, dimensions)?.path.clone(),
                    to: if slide.to.is_empty() {
                        None
                    } else {
                        Some(best(&slide.to, dimensions)?.path.clone())
                    },
                    progress: (delta - elapsed) / slide.duration,
                    interval: (slide.duration * 4.0 / 255.0).max(1.0),
                });
            }
            elapsed += slide.duration;
        }
        None
    }
}
fn best(variants: &[Variant], dimensions: [i32; 2]) -> Option<&Variant> {
    let aspect = f64::from(dimensions[0]) / f64::from(dimensions[1]);
    for large_only in [true, false] {
        let mut selected: Option<&Variant> = None;
        let mut distance = f64::MAX;
        for v in variants {
            if large_only && (v.dimensions[0] < dimensions[0] || v.dimensions[1] < dimensions[1]) {
                continue;
            }
            let d = (aspect - f64::from(v.dimensions[0]) / f64::from(v.dimensions[1])).abs();
            if d < distance
                || (d == distance
                    && selected.is_some_and(|old| {
                        (i64::from(v.dimensions[0]) - i64::from(dimensions[0])).abs()
                            < (i64::from(old.dimensions[0]) - i64::from(dimensions[0])).abs()
                    }))
            {
                distance = d;
                selected = Some(v);
            }
        }
        if selected.is_some() {
            return selected;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wall_clock_cycles_boundaries_and_negative_delta() {
        let t = Timeline {
            start: 100.0,
            total: 30.0,
            slides: vec![
                Slide {
                    duration: 10.0,
                    from: vec![Variant {
                        dimensions: [-1, -1],
                        path: "a".into(),
                    }],
                    to: vec![],
                },
                Slide {
                    duration: 20.0,
                    from: vec![Variant {
                        dimensions: [-1, -1],
                        path: "a".into(),
                    }],
                    to: vec![Variant {
                        dimensions: [-1, -1],
                        path: "b".into(),
                    }],
                },
            ],
        };
        assert_eq!(t.sample(110.0, [1280, 800]).unwrap().slide, 1);
        assert_eq!(t.sample(120.0, [1280, 800]).unwrap().progress, 0.5);
        assert_eq!(t.sample(130.0, [1280, 800]).unwrap().slide, 0);
        assert_eq!(t.sample(90.0, [1280, 800]).unwrap().progress, 0.5);
    }
    #[test]
    fn variants_prefer_large_aspect_then_width_and_reverse_ties() {
        let xml = b"<background><extension/><static><duration>1</duration><file><size width='1280' height='800'>first</size><size width='1280' height='800'>last</size><size width='2560' height='1600'>large</size></file></static></background>";
        let t = Timeline::parse(xml).unwrap();
        assert_eq!(t.total, f64::from(u32::MAX));
        assert_eq!(
            t.sample(t.start, [1280, 800]).unwrap().from,
            PathBuf::from("last")
        );
        assert_eq!(
            t.sample(t.start, [1920, 1200]).unwrap().from,
            PathBuf::from("large")
        );
    }
    #[test]
    fn bounded_invalid_timeline_does_not_expand_entities_or_accept_nan() {
        assert!(Timeline::parse(b"<!DOCTYPE background [<!ENTITY a 'x'>]><background/>").is_none());
        assert!(Timeline::parse(
            b"<background><static><duration>NaN</duration><file>a</file></static></background>"
        )
        .is_none());
        assert!(Timeline::parse(&vec![b' '; MAX_XML_BYTES + 1]).is_none());
    }
}
