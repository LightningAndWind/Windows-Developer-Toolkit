//! JSON 统计：节点数、最大深度、类型分布。

use super::JsonValue;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub nodes: usize,
    pub max_depth: usize,
    pub objects: usize,
    pub arrays: usize,
    pub strings: usize,
    pub numbers: usize,
    pub bools: usize,
    pub nulls: usize,
}

impl Stats {
    pub fn compute(v: &JsonValue) -> Stats {
        let mut s = Stats::default();
        walk(v, 1, &mut s);
        s
    }

    pub fn line(&self) -> String {
        format!(
            "节点 {} · 最大深度 {} · 对象 {} · 数组 {} · 字符串 {} · 数字 {} · 布尔 {} · 空 {}",
            self.nodes,
            self.max_depth,
            self.objects,
            self.arrays,
            self.strings,
            self.numbers,
            self.bools,
            self.nulls
        )
    }
}

fn walk(v: &JsonValue, depth: usize, s: &mut Stats) {
    s.nodes += 1;
    s.max_depth = s.max_depth.max(depth);
    match v {
        JsonValue::Null => s.nulls += 1,
        JsonValue::Bool(_) => s.bools += 1,
        JsonValue::Number(_) => s.numbers += 1,
        JsonValue::String(_) => s.strings += 1,
        JsonValue::Array(items) => {
            s.arrays += 1;
            for item in items {
                walk(item, depth + 1, s);
            }
        }
        JsonValue::Object(entries) => {
            s.objects += 1;
            for (_, val) in entries {
                walk(val, depth + 1, s);
            }
        }
    }
}
