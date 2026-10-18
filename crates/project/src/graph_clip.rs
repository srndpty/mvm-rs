//! グラフ clip の正。式は中立な数値 compiler で評価し、構文が不正な式も保存できる。

use std::collections::HashSet;

use crate::error::{ProjectResult, fail};
use crate::model::parse_argb_color;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct GraphFunctionId(pub String);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphViewport {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl Default for GraphViewport {
    fn default() -> Self {
        Self {
            x_min: -5.0,
            x_max: 5.0,
            y_min: -5.0,
            y_max: 5.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphAxes {
    pub show_axes: bool,
    pub show_grid: bool,
    pub x_label: String,
    pub y_label: String,
}

impl Default for GraphAxes {
    fn default() -> Self {
        Self {
            show_axes: true,
            show_grid: true,
            x_label: "x".to_owned(),
            y_label: "y".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphFunction {
    pub id: GraphFunctionId,
    pub expression: String,
    pub domain_min: Option<f64>,
    pub domain_max: Option<f64>,
    pub label: String,
    pub color: String,
    pub stroke_width: f64,
}

impl Default for GraphFunction {
    fn default() -> Self {
        Self {
            id: GraphFunctionId::default(),
            expression: "x^2".to_owned(),
            domain_min: None,
            domain_max: None,
            label: String::new(),
            color: "#FFFF6655".to_owned(),
            stroke_width: 3.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GraphIntroKind {
    #[default]
    None,
    Draw,
}

impl GraphIntroKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Draw => "draw",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::None, Self::Draw]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphIntro {
    pub kind: GraphIntroKind,
    pub frames: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphClipData {
    pub viewport: GraphViewport,
    pub axes: GraphAxes,
    pub functions: Vec<GraphFunction>,
    pub intro: GraphIntro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphValidationStatus {
    Valid,
    InvalidGraph,
    InvalidViewport,
}

/// 関数の数・文字列の長さ・Draw の尺の上限。
const MAXIMUM_GRAPH_FUNCTIONS: usize = 3;
const MAXIMUM_GRAPH_TEXT_BYTES: usize = 4096;
const MAXIMUM_GRAPH_INTRO_FRAMES: i64 = 10_000;

pub fn is_valid_graph_function_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

impl GraphClipData {
    /// 関数 1 つの既定の graph。
    pub fn with_default_function(id: GraphFunctionId) -> Self {
        Self {
            functions: vec![GraphFunction {
                id,
                ..GraphFunction::default()
            }],
            ..Self::default()
        }
    }

    /// `length` は clip の素材 frame 数。
    pub fn validate(&self, length: i64) -> GraphValidationStatus {
        use GraphValidationStatus::{InvalidGraph, InvalidViewport, Valid};
        let viewport = &self.viewport;
        let finite = [
            viewport.x_min,
            viewport.x_max,
            viewport.y_min,
            viewport.y_max,
        ]
        .iter()
        .all(|value| value.is_finite());
        if !finite
            || viewport.x_min >= viewport.x_max
            || viewport.y_min >= viewport.y_max
            || !(viewport.x_max - viewport.x_min).is_finite()
            || !(viewport.y_max - viewport.y_min).is_finite()
        {
            return InvalidViewport;
        }
        if length < 1
            || self.functions.is_empty()
            || self.functions.len() > MAXIMUM_GRAPH_FUNCTIONS
            || self.axes.x_label.len() > MAXIMUM_GRAPH_TEXT_BYTES
            || self.axes.y_label.len() > MAXIMUM_GRAPH_TEXT_BYTES
        {
            return InvalidGraph;
        }
        let mut ids = HashSet::new();
        for function in &self.functions {
            let low = function.domain_min.unwrap_or(viewport.x_min);
            let high = function.domain_max.unwrap_or(viewport.x_max);
            let valid = is_valid_graph_function_id(&function.id.0)
                && ids.insert(function.id.0.as_str())
                && low.is_finite()
                && high.is_finite()
                && low < high
                && low.max(viewport.x_min) < high.min(viewport.x_max)
                && function.stroke_width.is_finite()
                && function.stroke_width > 0.0
                && function.stroke_width <= 64.0
                && function.expression.len() <= MAXIMUM_GRAPH_TEXT_BYTES
                && function.label.len() <= MAXIMUM_GRAPH_TEXT_BYTES
                && parse_argb_color(&function.color).is_some();
            if !valid {
                return InvalidGraph;
            }
        }
        let intro_valid = match self.intro.kind {
            GraphIntroKind::None => self.intro.frames == 0,
            GraphIntroKind::Draw => {
                (1..=MAXIMUM_GRAPH_INTRO_FRAMES).contains(&self.intro.frames)
                    && self.intro.frames <= length
            }
        };
        if intro_valid { Valid } else { InvalidGraph }
    }

    fn change(&mut self, length: i64, edit: impl FnOnce(&mut Self) -> bool) -> ProjectResult<()> {
        if self.validate(length) != GraphValidationStatus::Valid {
            return fail("グラフが不正です");
        }
        let mut candidate = self.clone();
        if !edit(&mut candidate) || candidate.validate(length) != GraphValidationStatus::Valid {
            return fail("グラフを変更できません");
        }
        *self = candidate;
        Ok(())
    }

    fn function_index(&self, id: &GraphFunctionId) -> Option<usize> {
        self.functions
            .iter()
            .position(|function| function.id == *id)
    }

    pub fn add_function(&mut self, function: GraphFunction, length: i64) -> ProjectResult<()> {
        self.change(length, |candidate| {
            candidate.functions.push(function);
            true
        })
    }

    pub fn delete_function(&mut self, id: &GraphFunctionId, length: i64) -> ProjectResult<()> {
        self.change(length, |candidate| {
            let Some(index) = candidate.function_index(id) else {
                return false;
            };
            candidate.functions.remove(index);
            true
        })
    }

    pub fn move_function(
        &mut self,
        id: &GraphFunctionId,
        index: usize,
        length: i64,
    ) -> ProjectResult<()> {
        self.change(length, |candidate| {
            let Some(current) = candidate.function_index(id) else {
                return false;
            };
            if index >= candidate.functions.len() {
                return false;
            }
            let function = candidate.functions.remove(current);
            candidate.functions.insert(index, function);
            true
        })
    }

    pub fn update_function(&mut self, function: &GraphFunction, length: i64) -> ProjectResult<()> {
        self.change(length, |candidate| {
            let Some(index) = candidate.function_index(&function.id) else {
                return false;
            };
            candidate.functions[index] = function.clone();
            true
        })
    }

    /// 複製のために全ての関数 ID を発行し直す。発行は関数ごとに有限回で失敗する。
    pub fn remap_ids(&mut self, fresh: &mut dyn FnMut() -> String) -> ProjectResult<()> {
        const ATTEMPTS: usize = 128;
        let mut candidate = self.clone();
        let mut reserved: HashSet<String> = self
            .functions
            .iter()
            .map(|function| function.id.0.clone())
            .collect();
        for function in &mut candidate.functions {
            let id = (0..ATTEMPTS)
                .map(|_| fresh())
                .find(|id| is_valid_graph_function_id(id) && reserved.insert(id.clone()));
            let Some(id) = id else {
                return fail("Graph の一意な ID を発行できません");
            };
            function.id = GraphFunctionId(id);
        }
        *self = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> GraphClipData {
        GraphClipData::with_default_function(GraphFunctionId("f".to_owned()))
    }

    fn function(id: &str) -> GraphFunction {
        GraphFunction {
            id: GraphFunctionId(id.to_owned()),
            ..GraphFunction::default()
        }
    }

    #[test]
    fn default_graph_is_valid() {
        assert_eq!(graph().validate(1), GraphValidationStatus::Valid);
        assert_eq!(graph().validate(0), GraphValidationStatus::InvalidGraph);
    }

    #[test]
    fn rejects_bad_viewport_and_functions() {
        let mut viewport = graph();
        viewport.viewport.x_max = viewport.viewport.x_min;
        assert_eq!(
            viewport.validate(10),
            GraphValidationStatus::InvalidViewport
        );
        let mut overflow = graph();
        overflow.viewport.x_min = -f64::MAX;
        overflow.viewport.x_max = f64::MAX;
        assert_eq!(
            overflow.validate(10),
            GraphValidationStatus::InvalidViewport
        );

        let mut duplicate = graph();
        duplicate.functions.push(function("f"));
        assert_eq!(duplicate.validate(10), GraphValidationStatus::InvalidGraph);
        let mut too_many = graph();
        too_many
            .functions
            .extend([function("a"), function("b"), function("c")]);
        assert_eq!(too_many.validate(10), GraphValidationStatus::InvalidGraph);
        let mut outside = graph();
        outside.functions[0].domain_min = Some(6.0);
        outside.functions[0].domain_max = Some(7.0);
        assert_eq!(outside.validate(10), GraphValidationStatus::InvalidGraph);
        let mut bad_id = graph();
        bad_id.functions[0].id = GraphFunctionId("a b".to_owned());
        assert_eq!(bad_id.validate(10), GraphValidationStatus::InvalidGraph);
    }

    #[test]
    fn validates_draw_intro() {
        let mut draw = graph();
        draw.intro = GraphIntro {
            kind: GraphIntroKind::Draw,
            frames: 10,
        };
        assert_eq!(draw.validate(10), GraphValidationStatus::Valid);
        assert_eq!(draw.validate(9), GraphValidationStatus::InvalidGraph);
        draw.intro.frames = 0;
        assert_eq!(draw.validate(10), GraphValidationStatus::InvalidGraph);
    }

    #[test]
    fn edits_keep_graph_valid() {
        let mut data = graph();
        data.add_function(function("g"), 10).unwrap();
        data.move_function(&GraphFunctionId("g".to_owned()), 0, 10)
            .unwrap();
        assert_eq!(data.functions[0].id.0, "g");
        assert!(data.add_function(function("g"), 10).is_err());
        data.delete_function(&GraphFunctionId("f".to_owned()), 10)
            .unwrap();
        // 最後の関数は消せない（関数 0 個の graph は不正）。
        assert!(
            data.delete_function(&GraphFunctionId("g".to_owned()), 10)
                .is_err()
        );
        assert_eq!(data.functions.len(), 1);
    }

    #[test]
    fn remaps_ids_with_bounded_attempts() {
        let mut data = graph();
        let mut next = 0;
        data.remap_ids(&mut || {
            next += 1;
            format!("id{next}")
        })
        .unwrap();
        assert_eq!(data.functions[0].id.0, "id1");
        let mut stuck = graph();
        assert!(stuck.remap_ids(&mut || "f".to_owned()).is_err());
        assert_eq!(stuck, graph());
    }
}
