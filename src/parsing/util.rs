use fancy_regex::Captures;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Region {
  positions: Vec<Option<(usize,usize)>>
}

impl Region {
  pub fn new() -> Region {
    Region {
      positions: Vec::new()
    }
  }

  pub fn from_captures<'a>(captures: &Captures<'a>) -> Region {
    let mut region = Region::new();
    for i in 0..captures.len() {
      region.positions.push(captures.pos(i));
    }
    region
  }

  pub fn pos(&self, i: usize) -> Option<(usize,usize)> {
    if i < self.positions.len() {
      self.positions[i]
    } else {
      None
    }
  }
}
