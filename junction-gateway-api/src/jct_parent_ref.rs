use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct JctParentRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
}
