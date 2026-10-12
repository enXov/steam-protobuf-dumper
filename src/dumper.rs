use crate::collector::read_varint;
use crate::util;
use protobuf::descriptor::field_descriptor_proto::{Label, Type};
use protobuf::descriptor::{
    DescriptorProto, EnumDescriptorProto, EnumOptions, EnumValueOptions, ExtensionRangeOptions,
    FieldDescriptorProto, FieldOptions, FileDescriptorProto, FileOptions, MessageOptions,
    MethodOptions, OneofOptions, ServiceDescriptorProto, ServiceOptions,
};
use protobuf::UnknownValueRef;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;

// ─── Node types for the dependency graph ────────────────────────────────────

struct ProtoNode {
    name: String,
    proto: Option<FileDescriptorProto>,
    dependencies: Vec<String>,
    all_public_dependencies: HashSet<String>,
    types: Vec<String>,
    defined: bool,
}

struct ProtoTypeNode {
    #[allow(dead_code)]
    name: String,
    proto_name: Option<String>,
    source: TypeSource,
    defined: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
enum TypeSource {
    None,
    Field(FieldDescriptorProto),
    Enum(EnumDescriptorProto),
    Message(DescriptorProto),
    Service(ServiceDescriptorProto),
}

// ─── Main dumper ────────────────────────────────────────────────────────────

/// Analyzes protobuf dependency graphs and generates `.proto` source text.
///
/// Translation of the C# `ProtobufDumper` class.
pub struct ProtobufDumper {
    protobufs: Vec<FileDescriptorProto>,
    protobuf_map: HashMap<String, ProtoNode>,
    protobuf_type_map: HashMap<String, ProtoTypeNode>,
    /// Imports that must be added to the file currently being dumped because it
    /// uses options that only the binary's descriptor.proto defines. Interior
    /// mutability avoids threading state through every dump function.
    option_imports: RefCell<BTreeSet<String>>,
}

impl ProtobufDumper {
    pub fn new(protobufs: Vec<FileDescriptorProto>) -> Self {
        Self {
            protobufs,
            protobuf_map: HashMap::new(),
            protobuf_type_map: HashMap::new(),
            option_imports: RefCell::new(BTreeSet::new()),
        }
    }

    fn get_or_create_type_node(
        type_map: &mut HashMap<String, ProtoTypeNode>,
        name: &str,
        proto_name: Option<&str>,
        source: TypeSource,
    ) {
        let is_defining = !matches!(source, TypeSource::None);

        type_map
            .entry(name.to_string())
            .and_modify(|node| {
                if is_defining && !node.defined {
                    node.proto_name = proto_name.map(|s| s.to_string());
                    node.source = source.clone();
                    node.defined = true;
                }
            })
            .or_insert_with(|| ProtoTypeNode {
                name: name.to_string(),
                proto_name: proto_name.map(|s| s.to_string()),
                source,
                defined: is_defining,
            });
    }

    /// Builds the dependency graph, resolves all type references.
    /// Returns `true` if all dependencies and types are resolved.
    pub fn analyze(&mut self) -> bool {
        // Phase 1: Build nodes and register types
        for proto in &self.protobufs {
            let proto_name = proto.name().to_string();
            let package = proto.package().to_string();

            let mut type_names = Vec::new();

            // Extensions
            for ext in &proto.extension {
                let ext_type_name = get_package_path(&package, ext.name());
                Self::get_or_create_type_node(
                    &mut self.protobuf_type_map,
                    &ext_type_name,
                    Some(&proto_name),
                    TypeSource::Field(ext.clone()),
                );
                type_names.push(ext_type_name);

                if is_named_type(ext.type_()) && !ext.type_name().is_empty() {
                    let tn = get_package_path(&package, ext.type_name());
                    Self::get_or_create_type_node(
                        &mut self.protobuf_type_map,
                        &tn,
                        None,
                        TypeSource::None,
                    );
                    type_names.push(tn);
                }

                if !ext.extendee().is_empty() {
                    let tn = get_package_path(&package, ext.extendee());
                    Self::get_or_create_type_node(
                        &mut self.protobuf_type_map,
                        &tn,
                        None,
                        TypeSource::None,
                    );
                    type_names.push(tn);
                }
            }

            // Enums
            for enum_type in &proto.enum_type {
                let tn = get_package_path(&package, enum_type.name());
                Self::get_or_create_type_node(
                    &mut self.protobuf_type_map,
                    &tn,
                    Some(&proto_name),
                    TypeSource::Enum(enum_type.clone()),
                );
                type_names.push(tn);
            }

            // Messages
            for msg in &proto.message_type {
                Self::recursive_analyze_message(
                    &mut self.protobuf_type_map,
                    msg,
                    &proto_name,
                    &package,
                    &mut type_names,
                );
            }

            // Services
            for service in &proto.service {
                let tn = get_package_path(&package, service.name());
                Self::get_or_create_type_node(
                    &mut self.protobuf_type_map,
                    &tn,
                    Some(&proto_name),
                    TypeSource::Service(service.clone()),
                );
                type_names.push(tn);

                for method in &service.method {
                    if !method.input_type().is_empty() {
                        let tn = get_package_path(&package, method.input_type());
                        Self::get_or_create_type_node(
                            &mut self.protobuf_type_map,
                            &tn,
                            None,
                            TypeSource::None,
                        );
                        type_names.push(tn);
                    }
                    if !method.output_type().is_empty() {
                        let tn = get_package_path(&package, method.output_type());
                        Self::get_or_create_type_node(
                            &mut self.protobuf_type_map,
                            &tn,
                            None,
                            TypeSource::None,
                        );
                        type_names.push(tn);
                    }
                }
            }

            let node = ProtoNode {
                name: proto_name.clone(),
                proto: Some(proto.clone()),
                dependencies: Vec::new(),
                all_public_dependencies: HashSet::new(),
                types: type_names,
                defined: true,
            };

            self.protobuf_map.insert(proto_name, node);
        }

        // Phase 2: Resolve file dependencies
        let proto_names: Vec<String> = self.protobuf_map.keys().cloned().collect();
        let mut missing_deps: Vec<String> = Vec::new();

        for name in &proto_names {
            let deps: Vec<String> = self.protobuf_map[name]
                .proto
                .as_ref()
                .map(|p| p.dependency.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();

            let mut resolved = Vec::new();
            for dep in &deps {
                if dep.starts_with("google") {
                    continue;
                }

                if self.protobuf_map.contains_key(dep) {
                    resolved.push(dep.clone());
                } else {
                    log::warn!(
                        "Unknown dependency: {} for {}",
                        dep,
                        self.protobuf_map[name].name
                    );

                    if !missing_deps.contains(dep) {
                        missing_deps.push(dep.clone());
                    }
                    resolved.push(dep.clone());
                }
            }

            if let Some(node) = self.protobuf_map.get_mut(name) {
                node.dependencies = resolved;
            }
        }

        // Add missing dependency placeholders
        for dep_name in &missing_deps {
            self.protobuf_map.insert(
                dep_name.clone(),
                ProtoNode {
                    name: dep_name.clone(),
                    proto: None,
                    dependencies: Vec::new(),
                    all_public_dependencies: HashSet::new(),
                    types: Vec::new(),
                    defined: false,
                },
            );
        }

        // Phase 3: Check all dependencies are defined
        let all_names: Vec<String> = self.protobuf_map.keys().cloned().collect();
        for name in &all_names {
            let node = &self.protobuf_map[name];

            let undefined_deps: Vec<String> = node
                .dependencies
                .iter()
                .filter(|d| self.protobuf_map.get(*d).is_none_or(|n| !n.defined))
                .cloned()
                .collect();

            if !undefined_deps.is_empty() {
                log::error!("Not all dependencies were found for {}", name);
                for dep in &undefined_deps {
                    log::error!("Dependency not found: {}", dep);
                }
                return false;
            }

            let undefined_types: Vec<String> = node
                .types
                .iter()
                .filter(|t| self.protobuf_type_map.get(*t).is_none_or(|n| !n.defined))
                .cloned()
                .collect();

            if !undefined_types.is_empty() {
                log::error!("Not all types were resolved for {}", name);
                for t in &undefined_types {
                    log::error!("Type not found: {}", t);
                }
                return false;
            }
        }

        // Phase 4: Build public dependency sets
        for name in &all_names {
            let mut pub_deps = HashSet::new();
            self.recursive_add_public_dependencies(&mut pub_deps, name, 0);
            if let Some(node) = self.protobuf_map.get_mut(name) {
                node.all_public_dependencies = pub_deps;
            }
        }

        true
    }

    fn recursive_analyze_message(
        type_map: &mut HashMap<String, ProtoTypeNode>,
        msg: &DescriptorProto,
        proto_name: &str,
        package_path: &str,
        type_names: &mut Vec<String>,
    ) {
        let tn = get_package_path(package_path, msg.name());
        Self::get_or_create_type_node(
            type_map,
            &tn,
            Some(proto_name),
            TypeSource::Message(msg.clone()),
        );
        type_names.push(tn);

        for ext in &msg.extension {
            // Extensions can be declared inside a message; register them under
            // their full path so their options use the full name as the key
            let etn = get_package_path(&get_package_path(package_path, msg.name()), ext.name());
            Self::get_or_create_type_node(
                type_map,
                &etn,
                Some(proto_name),
                TypeSource::Field(ext.clone()),
            );
            type_names.push(etn);

            if is_named_type(ext.type_()) && !ext.type_name().is_empty() {
                let tn = get_package_path(package_path, ext.type_name());
                Self::get_or_create_type_node(type_map, &tn, None, TypeSource::None);
                type_names.push(tn);
            }

            if !ext.extendee().is_empty() {
                let etn = get_package_path(package_path, ext.extendee());
                Self::get_or_create_type_node(type_map, &etn, None, TypeSource::None);
                type_names.push(etn);
            }
        }

        for enum_type in &msg.enum_type {
            let etn = get_package_path(
                &get_package_path(package_path, msg.name()),
                enum_type.name(),
            );
            Self::get_or_create_type_node(
                type_map,
                &etn,
                Some(proto_name),
                TypeSource::Enum(enum_type.clone()),
            );
            type_names.push(etn);
        }

        for field in &msg.field {
            if is_named_type(field.type_()) && !field.type_name().is_empty() {
                let ftn = get_package_path(package_path, field.type_name());
                Self::get_or_create_type_node(type_map, &ftn, None, TypeSource::None);
                type_names.push(ftn);
            }
            if !field.extendee().is_empty() {
                let ftn = get_package_path(package_path, field.extendee());
                Self::get_or_create_type_node(type_map, &ftn, None, TypeSource::None);
                type_names.push(ftn);
            }
        }

        for nested in &msg.nested_type {
            Self::recursive_analyze_message(
                type_map,
                nested,
                proto_name,
                &get_package_path(package_path, msg.name()),
                type_names,
            );
        }
    }

    fn recursive_add_public_dependencies(
        &self,
        set: &mut HashSet<String>,
        node_name: &str,
        depth: usize,
    ) {
        let node = match self.protobuf_map.get(node_name) {
            Some(n) => n,
            None => return,
        };

        let proto = match &node.proto {
            Some(p) => p,
            None => return,
        };

        if depth == 0 {
            for dep in &proto.dependency {
                // Google dependencies are not required to be in the binary
                if !self.is_dependency_defined(dep) {
                    continue;
                }
                set.insert(dep.to_string());
                self.recursive_add_public_dependencies(set, dep, depth + 1);
            }
        } else {
            for &idx in &proto.public_dependency {
                let Some(dep) = proto.dependency.get(idx as usize) else {
                    continue;
                };
                if !self.is_dependency_defined(dep) {
                    continue;
                }
                set.insert(dep.to_string());
                self.recursive_add_public_dependencies(set, dep, depth + 1);
            }
        }
    }

    fn is_dependency_defined(&self, name: &str) -> bool {
        self.protobuf_map.get(name).is_some_and(|n| n.defined)
    }

    // ─── File generation ────────────────────────────────────────────────────

    /// Generates `.proto` source text for each collected protobuf descriptor.
    /// Calls `callback(name, content)` for each file.
    pub fn dump_files<F: FnMut(&str, &str)>(&self, mut callback: F) {
        for proto in &self.protobufs {
            let mut sb = String::new();
            self.dump_file_descriptor(proto, &mut sb);
            callback(proto.name(), &sb);
        }
    }

    fn dump_file_descriptor(&self, proto: &FileDescriptorProto, sb: &mut String) {
        let mut marker = false;

        // Syntax
        if !proto.syntax().is_empty() {
            append_heading_space(sb, &mut marker);
            let _ = writeln!(sb, "syntax = {};", util::to_literal(proto.syntax()));
            marker = true;
        }

        // Imports
        for (i, dep) in proto.dependency.iter().enumerate() {
            let modifier = if proto.public_dependency.contains(&(i as i32)) {
                "public "
            } else if proto.weak_dependency.contains(&(i as i32)) {
                "weak "
            } else {
                ""
            };
            let _ = writeln!(sb, "import {modifier}\"{dep}\";");
            marker = true;
        }

        let imports_end = sb.len();
        let has_heading = marker;
        self.option_imports.borrow_mut().clear();

        // Package
        if !proto.package().is_empty() {
            append_heading_space(sb, &mut marker);
            let _ = writeln!(sb, "package {};", proto.package());
            marker = true;
        }

        // File options
        let options = self.dump_file_options(proto, proto.options.as_ref());
        for (key, value) in &options {
            append_heading_space(sb, &mut marker);
            let _ = writeln!(sb, "option {key} = {value};");
        }
        if !options.is_empty() {
            marker = true;
        }

        // Extensions
        self.dump_extension_descriptors(proto, &proto.extension, sb, 0, &mut marker);

        // Enums
        for enum_type in &proto.enum_type {
            self.dump_enum_descriptor(proto, enum_type, sb, 0, &mut marker);
        }

        // Messages
        for msg in &proto.message_type {
            self.dump_descriptor(proto, msg, sb, 0, &mut marker);
        }

        // Services
        for service in &proto.service {
            self.dump_service(proto, service, sb, &mut marker);
        }

        // Valve's protoc has the options of its descriptor.proto built in, so the binary doesn't
        // list the import, but any other protoc only knows them when the file imports it
        let option_imports = self.option_imports.borrow();
        if !option_imports.is_empty() {
            let mut imports = String::new();

            for import in option_imports.iter() {
                let _ = writeln!(imports, "import \"{import}\";");
            }

            if !has_heading && imports_end < sb.len() {
                imports.push('\n');
            }

            sb.insert_str(imports_end, &imports);
        }
    }

    fn dump_extension_descriptors(
        &self,
        source: &FileDescriptorProto,
        fields: &[FieldDescriptorProto],
        sb: &mut String,
        level: usize,
        marker: &mut bool,
    ) {
        // Group by extendee — BTreeMap ensures deterministic output order
        let mut groups: BTreeMap<&str, Vec<&FieldDescriptorProto>> = BTreeMap::new();
        for field in fields {
            if field.extendee().is_empty() {
                continue;
            }
            groups.entry(field.extendee()).or_default().push(field);
        }

        let indent = "\t".repeat(level);
        for (extendee, ext_fields) in &groups {
            append_heading_space(sb, marker);
            let _ = writeln!(sb, "{indent}extend {extendee} {{");

            for field in ext_fields {
                let decl = self.build_descriptor_declaration(source, field, true);
                let _ = writeln!(sb, "{indent}\t{decl}");
            }

            let _ = writeln!(sb, "{indent}}}");
            *marker = true;
        }
    }

    fn dump_descriptor(
        &self,
        source: &FileDescriptorProto,
        proto: &DescriptorProto,
        sb: &mut String,
        level: usize,
        marker: &mut bool,
    ) {
        let indent = "\t".repeat(level);
        let mut inner_marker = false;

        append_heading_space(sb, marker);
        let _ = writeln!(sb, "{indent}message {} {{", proto.name());

        // Message options
        let options = self.dump_message_options(source, proto.options.as_ref());
        for (key, value) in &options {
            append_heading_space(sb, &mut inner_marker);
            let _ = writeln!(sb, "{indent}\toption {key} = {value};");
        }
        if !options.is_empty() {
            inner_marker = true;
        }

        // Nested extensions
        if !proto.extension.is_empty() {
            self.dump_extension_descriptors(
                source,
                &proto.extension,
                sb,
                level + 1,
                &mut inner_marker,
            );
        }

        // Nested messages — map entries are written as the map fields that use them
        for nested in &proto.nested_type {
            if nested.options.as_ref().is_some_and(|o| o.map_entry()) {
                continue;
            }
            self.dump_descriptor(source, nested, sb, level + 1, &mut inner_marker);
        }

        // Nested enums
        for enum_type in &proto.enum_type {
            self.dump_enum_descriptor(source, enum_type, sb, level + 1, &mut inner_marker);
        }

        // Root fields (not part of a oneof); proto3 optional fields are each in a
        // oneof of their own that the compiler made, so keep them with the root fields
        let root_fields: Vec<&FieldDescriptorProto> = proto
            .field
            .iter()
            .filter(|f| !f.has_oneof_index() || f.proto3_optional())
            .collect();

        for field in &root_fields {
            append_heading_space(sb, &mut inner_marker);
            let decl = self.build_descriptor_declaration(source, field, true);
            let _ = writeln!(sb, "{indent}\t{decl}");
        }
        if !root_fields.is_empty() {
            inner_marker = true;
        }

        // Oneofs
        for (i, oneof) in proto.oneof_decl.iter().enumerate() {
            let oneof_fields: Vec<&FieldDescriptorProto> = proto
                .field
                .iter()
                .filter(|f| f.has_oneof_index() && f.oneof_index() == i as i32)
                .collect();

            // Synthetic oneofs that wrap proto3 optional fields are not real oneofs
            if oneof_fields.iter().any(|f| f.proto3_optional()) {
                continue;
            }

            append_heading_space(sb, &mut inner_marker);
            let _ = writeln!(sb, "{indent}\toneof {} {{", oneof.name());

            for (key, value) in self.dump_oneof_options(source, oneof.options.as_ref()) {
                let _ = writeln!(sb, "{indent}\t\toption {key} = {value};");
            }

            for field in &oneof_fields {
                let decl = self.build_descriptor_declaration(source, field, false);
                let _ = writeln!(sb, "{indent}\t\t{decl}");
            }

            let _ = writeln!(sb, "{indent}\t}}");
            inner_marker = true;
        }

        // Extension ranges — range ends in messages are exclusive
        for range in &proto.extension_range {
            let range_options = self.dump_ext_range_options(source, range.options.as_ref());
            let parameters = if range_options.is_empty() {
                String::new()
            } else {
                let opts: Vec<String> = range_options
                    .iter()
                    .map(|(k, v)| format!("{k} = {v}"))
                    .collect();
                format!(" [{}]", opts.join(", "))
            };

            append_heading_space(sb, &mut inner_marker);
            let _ = writeln!(
                sb,
                "{indent}\textensions {}{parameters};",
                format_range(range.start(), range.end() - 1, MAX_FIELD_NUMBER)
            );
        }

        dump_reserved(
            proto
                .reserved_range
                .iter()
                .map(|r| format_range(r.start(), r.end() - 1, MAX_FIELD_NUMBER)),
            &proto.reserved_name,
            sb,
            &indent,
            &mut inner_marker,
        );

        let _ = writeln!(sb, "{indent}}}");
        *marker = true;
    }

    fn dump_enum_descriptor(
        &self,
        source: &FileDescriptorProto,
        field: &EnumDescriptorProto,
        sb: &mut String,
        level: usize,
        marker: &mut bool,
    ) {
        let indent = "\t".repeat(level);

        append_heading_space(sb, marker);
        let _ = writeln!(sb, "{indent}enum {} {{", field.name());

        // Enum options
        for (key, value) in self.dump_enum_options(source, field.options.as_ref()) {
            let _ = writeln!(sb, "{indent}\toption {key} = {value};");
        }

        // Enum values
        for enum_value in &field.value {
            let options = self.dump_enum_value_options(source, enum_value.options.as_ref());
            let params = if options.is_empty() {
                String::new()
            } else {
                let opts: Vec<String> = options.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                format!(" [{}]", opts.join(", "))
            };

            let _ = writeln!(
                sb,
                "{indent}\t{} = {}{params};",
                enum_value.name(),
                enum_value.number()
            );
        }

        // Reserved ranges — range ends in enums are inclusive
        let mut inner_marker = false;
        dump_reserved(
            field
                .reserved_range
                .iter()
                .map(|r| format_range(r.start(), r.end(), i32::MAX)),
            &field.reserved_name,
            sb,
            &indent,
            &mut inner_marker,
        );

        let _ = writeln!(sb, "{indent}}}");
        *marker = true;
    }

    fn dump_service(
        &self,
        source: &FileDescriptorProto,
        service: &ServiceDescriptorProto,
        sb: &mut String,
        marker: &mut bool,
    ) {
        let mut inner_marker = false;

        append_heading_space(sb, marker);
        let _ = writeln!(sb, "service {} {{", service.name());

        // Service options
        let root_options = self.dump_service_options(source, service.options.as_ref());
        for (key, value) in &root_options {
            let _ = writeln!(sb, "\toption {key} = {value};");
        }
        if !root_options.is_empty() {
            inner_marker = true;
        }

        // Methods
        for method in &service.method {
            let client_stream = if method.client_streaming() {
                "stream "
            } else {
                ""
            };
            let server_stream = if method.server_streaming() {
                "stream "
            } else {
                ""
            };
            let declaration = format!(
                "\trpc {} ({client_stream}{}) returns ({server_stream}{})",
                method.name(),
                method.input_type(),
                method.output_type()
            );

            let options = self.dump_method_options(source, method.options.as_ref());

            append_heading_space(sb, &mut inner_marker);

            if options.is_empty() {
                let _ = writeln!(sb, "{declaration};");
            } else {
                let _ = writeln!(sb, "{declaration} {{");
                for (key, value) in &options {
                    let _ = writeln!(sb, "\t\toption {key} = {value};");
                }
                let _ = writeln!(sb, "\t}}");
                inner_marker = true;
            }
        }

        let _ = writeln!(sb, "}}");
        *marker = true;
    }

    fn build_descriptor_declaration(
        &self,
        source: &FileDescriptorProto,
        field: &FieldDescriptorProto,
        emit_field_label: bool,
    ) -> String {
        let mut type_name = resolve_type(field);
        let mut options: Vec<(String, String)> = Vec::new();
        let is_proto2 = source.syntax().is_empty() || source.syntax() == "proto2";

        // Default value — the presence check keeps empty string defaults
        if field.has_default_value() {
            // Strings are stored as is, bytes are already escaped
            let default = match field.type_() {
                Type::TYPE_STRING => util::to_literal(field.default_value()),
                Type::TYPE_BYTES => format!("\"{}\"", field.default_value()),
                _ => field.default_value().to_string(),
            };
            options.push(("default".to_string(), default));
        } else if is_proto2
            && field.type_() == Type::TYPE_ENUM
            && field.label() != Label::LABEL_REPEATED
        {
            // Proto2 tracks the first enum value as the implicit default
            if let Some(type_node) = self.protobuf_type_map.get(field.type_name()) {
                if let TypeSource::Enum(ref enum_desc) = type_node.source {
                    if let Some(first_val) = enum_desc.value.first() {
                        options.push(("default".to_string(), first_val.name().to_string()));
                    }
                }
            }
        }

        // JSON name
        if !field.json_name().is_empty() {
            options.push(("json_name".to_string(), util::to_literal(field.json_name())));
        }

        // Field options
        let field_options = self.dump_field_options(source, field.options.as_ref());
        for (k, v) in field_options {
            options.push((k, v));
        }

        let params = if options.is_empty() {
            String::new()
        } else {
            let opts: Vec<String> = options.iter().map(|(k, v)| format!("{k} = {v}")).collect();
            format!(" [{}]", opts.join(", "))
        };

        let mut label: Option<&'static str> = None;

        let mut is_map = false;
        if field.label() == Label::LABEL_REPEATED && field.type_() == Type::TYPE_MESSAGE {
            if let Some(node) = self.protobuf_type_map.get(field.type_name()) {
                if let TypeSource::Message(ref map_entry) = node.source {
                    if map_entry.options.as_ref().is_some_and(|o| o.map_entry()) {
                        let key = map_entry.field.iter().find(|f| f.number() == 1);
                        let value = map_entry.field.iter().find(|f| f.number() == 2);
                        if let (Some(key), Some(value)) = (key, value) {
                            type_name =
                                format!("map<{}, {}>", resolve_type(key), resolve_type(value));
                            is_map = true;
                        }
                    }
                }
            }
        }

        // Map fields carry no label
        if !is_map && emit_field_label {
            // Fields in proto3 have no label unless they are repeated or track presence
            if is_proto2 || field.label() == Label::LABEL_REPEATED || field.proto3_optional() {
                label = Some(get_label(field.label()));
            }
        }

        let mut decl = String::new();
        if let Some(label) = label {
            let _ = write!(decl, "{label} ");
        }
        let _ = write!(
            decl,
            "{type_name} {} = {}{params};",
            field.name(),
            field.number()
        );

        decl
    }

    // ─── Options dumping ────────────────────────────────────────────────────

    fn dump_file_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&FileOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }
        if opts.has_optimize_for() {
            kv.push(("optimize_for".into(), format!("{:?}", opts.optimize_for())));
        }
        if opts.has_cc_generic_services() {
            kv.push((
                "cc_generic_services".into(),
                bool_str(opts.cc_generic_services()),
            ));
        }
        if opts.has_cc_enable_arenas() {
            kv.push(("cc_enable_arenas".into(), bool_str(opts.cc_enable_arenas())));
        }
        if opts.has_go_package() {
            kv.push(("go_package".into(), util::to_literal(opts.go_package())));
        }
        if opts.has_java_package() {
            kv.push(("java_package".into(), util::to_literal(opts.java_package())));
        }
        if opts.has_java_outer_classname() {
            kv.push((
                "java_outer_classname".into(),
                util::to_literal(opts.java_outer_classname()),
            ));
        }
        if opts.has_java_generate_equals_and_hash() {
            kv.push((
                "java_generate_equals_and_hash".into(),
                bool_str(opts.java_generate_equals_and_hash()),
            ));
        }
        if opts.has_java_generic_services() {
            kv.push((
                "java_generic_services".into(),
                bool_str(opts.java_generic_services()),
            ));
        }
        if opts.has_java_multiple_files() {
            kv.push((
                "java_multiple_files".into(),
                bool_str(opts.java_multiple_files()),
            ));
        }
        if opts.has_java_string_check_utf8() {
            kv.push((
                "java_string_check_utf8".into(),
                bool_str(opts.java_string_check_utf8()),
            ));
        }
        if opts.has_py_generic_services() {
            kv.push((
                "py_generic_services".into(),
                bool_str(opts.py_generic_services()),
            ));
        }
        if opts.has_ruby_package() {
            kv.push(("ruby_package".into(), util::to_literal(opts.ruby_package())));
        }
        if opts.has_objc_class_prefix() {
            kv.push((
                "objc_class_prefix".into(),
                util::to_literal(opts.objc_class_prefix()),
            ));
        }
        if opts.has_csharp_namespace() {
            kv.push((
                "csharp_namespace".into(),
                util::to_literal(opts.csharp_namespace()),
            ));
        }
        if opts.has_swift_prefix() {
            kv.push(("swift_prefix".into(), util::to_literal(opts.swift_prefix())));
        }
        if opts.has_php_generic_services() {
            kv.push((
                "php_generic_services".into(),
                bool_str(opts.php_generic_services()),
            ));
        }
        if opts.has_php_class_prefix() {
            kv.push((
                "php_class_prefix".into(),
                util::to_literal(opts.php_class_prefix()),
            ));
        }
        if opts.has_php_namespace() {
            kv.push((
                "php_namespace".into(),
                util::to_literal(opts.php_namespace()),
            ));
        }
        if opts.has_php_metadata_namespace() {
            kv.push((
                "php_metadata_namespace".into(),
                util::to_literal(opts.php_metadata_namespace()),
            ));
        }

        self.dump_options_extensions(source, ".google.protobuf.FileOptions", opts, &mut kv);
        kv
    }

    fn dump_field_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&FieldOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_ctype() {
            kv.push(("ctype".into(), format!("{:?}", opts.ctype())));
        }
        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }
        if opts.has_lazy() {
            kv.push(("lazy".into(), bool_str(opts.lazy())));
        }
        if opts.has_packed() {
            kv.push(("packed".into(), bool_str(opts.packed())));
        }
        if opts.has_weak() {
            kv.push(("weak".into(), bool_str(opts.weak())));
        }
        if opts.has_jstype() {
            kv.push(("jstype".into(), format!("{:?}", opts.jstype())));
        }

        self.dump_options_extensions(source, ".google.protobuf.FieldOptions", opts, &mut kv);
        kv
    }

    fn dump_message_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&MessageOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_message_set_wire_format() {
            kv.push((
                "message_set_wire_format".into(),
                bool_str(opts.message_set_wire_format()),
            ));
        }
        if opts.has_no_standard_descriptor_accessor() {
            kv.push((
                "no_standard_descriptor_accessor".into(),
                bool_str(opts.no_standard_descriptor_accessor()),
            ));
        }
        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }

        self.dump_options_extensions(source, ".google.protobuf.MessageOptions", opts, &mut kv);
        kv
    }

    fn dump_enum_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&EnumOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_allow_alias() {
            kv.push(("allow_alias".into(), bool_str(opts.allow_alias())));
        }
        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }

        self.dump_options_extensions(source, ".google.protobuf.EnumOptions", opts, &mut kv);
        kv
    }

    fn dump_enum_value_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&EnumValueOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }

        self.dump_options_extensions(source, ".google.protobuf.EnumValueOptions", opts, &mut kv);
        kv
    }

    fn dump_service_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&ServiceOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }

        self.dump_options_extensions(source, ".google.protobuf.ServiceOptions", opts, &mut kv);
        kv
    }

    fn dump_method_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&MethodOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        if opts.has_deprecated() {
            kv.push(("deprecated".into(), bool_str(opts.deprecated())));
        }
        if opts.has_idempotency_level() {
            kv.push((
                "idempotency_level".into(),
                format!("{:?}", opts.idempotency_level()),
            ));
        }

        self.dump_options_extensions(source, ".google.protobuf.MethodOptions", opts, &mut kv);
        kv
    }

    fn dump_oneof_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&OneofOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        self.dump_options_extensions(source, ".google.protobuf.OneofOptions", opts, &mut kv);
        kv
    }

    fn dump_ext_range_options(
        &self,
        source: &FileDescriptorProto,
        options: Option<&ExtensionRangeOptions>,
    ) -> Vec<(String, String)> {
        let mut kv = Vec::new();
        let Some(opts) = options else {
            return kv;
        };

        self.dump_options_extensions(
            source,
            ".google.protobuf.ExtensionRangeOptions",
            opts,
            &mut kv,
        );
        kv
    }

    // ─── Extension option extraction ────────────────────────────────────────

    /// Reads custom extension options from the unknown fields of a protobuf options message.
    ///
    /// This is the Rust equivalent of the C# `DumpOptionsMatching` + `DumpOptionsFieldRecursive`.
    /// The `protobuf` crate preserves unknown fields in `SpecialFields`, which is how
    /// extension data survives deserialization.
    fn dump_options_extensions<M: protobuf::Message>(
        &self,
        source: &FileDescriptorProto,
        type_name: &str,
        options: &M,
        kv: &mut Vec<(String, String)>,
    ) {
        let source_name = source.name();
        let dependencies = match self.protobuf_map.get(source_name) {
            Some(node) => {
                let mut deps = node.all_public_dependencies.clone();
                deps.insert(source_name.to_string());
                deps
            }
            None => return,
        };

        let values = raw_values_of(options);

        // Options that the binary's descriptor.proto has but the descriptor message types do
        // not, such as the ones Valve added, are left over as unknown fields. The fields the
        // types do have were read into their properties and are dumped by the code above.
        if let Some(type_node) = self.protobuf_type_map.get(type_name) {
            if let TypeSource::Message(ref options_proto) = type_node.source {
                let count = kv.len();

                for field in &options_proto.field {
                    if field.name() != "uninterpreted_option" {
                        self.dump_options_field_recursive(field, &values, kv, field.name());
                    }
                }

                if kv.len() > count {
                    if let Some(proto_name) = &type_node.proto_name {
                        if !dependencies.contains(proto_name) {
                            self.option_imports.borrow_mut().insert(proto_name.clone());
                        }
                    }
                }
            }
        }

        // Find all extension fields that extend this options type.
        // Sort by field number (path as tiebreak) for deterministic output order
        // (HashMap iteration is random).
        let mut sorted_types: Vec<_> = self.protobuf_type_map.iter().collect();
        sorted_types.sort_by(|(path_a, a), (path_b, b)| {
            let number = |node: &ProtoTypeNode| {
                if let TypeSource::Field(ref field) = node.source {
                    field.number()
                } else {
                    0
                }
            };
            number(a).cmp(&number(b)).then_with(|| path_a.cmp(path_b))
        });
        for (type_path, type_node) in sorted_types {
            let proto_name = match &type_node.proto_name {
                Some(n) => n,
                None => continue,
            };

            if !dependencies.contains(proto_name) {
                continue;
            }

            if let TypeSource::Field(ref field) = type_node.source {
                if !field.extendee().is_empty() && field.extendee() == type_name {
                    // The full name, extensions can be declared inside a message or a package
                    let key = format!("({})", type_path.trim_start_matches('.'));
                    self.dump_options_field_recursive(field, &values, kv, &key);
                }
            }
        }
    }

    fn dump_options_field_recursive(
        &self,
        field: &FieldDescriptorProto,
        values: &[(u32, RawValue)],
        kv: &mut Vec<(String, String)>,
        key: &str,
    ) {
        // A message that is not repeated is written field by field, a repeated one is
        // written as an aggregate value per element
        if field.type_() == Type::TYPE_MESSAGE && field.label() != Label::LABEL_REPEATED {
            let mut bytes: Option<&[u8]> = None;
            for (num, val) in values {
                if *num != field.number() as u32 {
                    continue;
                }
                if let RawValue::LengthDelimited(b) = val {
                    bytes = Some(b);
                }
            }
            let Some(bytes) = bytes else { return };

            let Some(sub_values) = parse_wire_values(bytes) else {
                return;
            };

            let count = kv.len();
            if let Some(type_node) = self.protobuf_type_map.get(field.type_name()) {
                if let TypeSource::Message(ref msg_proto) = type_node.source {
                    for sub_field in &msg_proto.field {
                        let sub_key = format!("{key}.{}", sub_field.name());
                        self.dump_options_field_recursive(sub_field, &sub_values, kv, &sub_key);
                    }
                }
            }

            if kv.len() == count {
                kv.push((key.to_string(), "{}".to_string()));
            }

            return;
        }

        for value in self.get_option_values(values, field) {
            kv.push((key.to_string(), value));
        }
    }

    /// Returns the values of a field in an options message; a field that is not
    /// repeated keeps its last value. Translation of the C# `GetOptionValues`.
    fn get_option_values(
        &self,
        values: &[(u32, RawValue)],
        field: &FieldDescriptorProto,
    ) -> Vec<String> {
        let field_num = field.number() as u32;
        let mut out: Vec<String> = Vec::new();

        for (num, val) in values {
            if *num != field_num {
                continue;
            }

            let value = match (field.type_(), val) {
                (Type::TYPE_INT32, RawValue::Varint(v)) => Some((*v as u32 as i32).to_string()),
                // Zigzag is not decoded for wire-level values
                (Type::TYPE_SINT32, RawValue::Varint(v)) => {
                    let x = *v as u32;
                    Some(((x >> 1) as i32 ^ -((x & 1) as i32)).to_string())
                }
                (Type::TYPE_SFIXED32, RawValue::Fixed32(v)) => Some((*v as i32).to_string()),
                (Type::TYPE_UINT32, RawValue::Varint(v)) => Some((*v as u32).to_string()),
                (Type::TYPE_FIXED32, RawValue::Fixed32(v)) => Some(v.to_string()),
                (Type::TYPE_INT64, RawValue::Varint(v)) => Some((*v as i64).to_string()),
                (Type::TYPE_SINT64, RawValue::Varint(v)) => {
                    let x = *v;
                    Some(((x >> 1) as i64 ^ -((x & 1) as i64)).to_string())
                }
                (Type::TYPE_SFIXED64, RawValue::Fixed64(v)) => Some((*v as i64).to_string()),
                (Type::TYPE_UINT64, RawValue::Varint(v)) => Some(v.to_string()),
                (Type::TYPE_FIXED64, RawValue::Fixed64(v)) => Some(v.to_string()),
                (Type::TYPE_BOOL, RawValue::Varint(v)) => {
                    Some(if *v != 0 { "true" } else { "false" }.to_string())
                }
                (Type::TYPE_FLOAT, RawValue::Fixed32(v)) => {
                    Some(util::to_literal_f32(f32::from_bits(*v)))
                }
                (Type::TYPE_DOUBLE, RawValue::Fixed64(v)) => {
                    Some(util::to_literal_f64(f64::from_bits(*v)))
                }
                (Type::TYPE_STRING, RawValue::LengthDelimited(bytes)) => {
                    std::str::from_utf8(bytes).ok().map(util::to_literal)
                }
                (Type::TYPE_BYTES, RawValue::LengthDelimited(bytes)) => {
                    Some(util::to_literal_bytes(bytes))
                }
                (Type::TYPE_ENUM, RawValue::Varint(v)) => {
                    Some(self.get_enum_value_name(field, *v as u32 as i32))
                }
                (Type::TYPE_MESSAGE, RawValue::LengthDelimited(bytes)) => {
                    let Some(type_node) = self.protobuf_type_map.get(field.type_name()) else {
                        continue;
                    };
                    let TypeSource::Message(ref msg_proto) = type_node.source else {
                        continue;
                    };
                    let Some(sub_values) = parse_wire_values(bytes) else {
                        continue;
                    };
                    Some(self.build_aggregate_value(msg_proto, &sub_values))
                }
                _ => None,
            };

            if let Some(value) = value {
                out.push(value);
            }
        }

        if field.label() != Label::LABEL_REPEATED && out.len() > 1 {
            out.drain(..out.len() - 1);
        }

        out
    }

    fn get_enum_value_name(&self, field: &FieldDescriptorProto, number: i32) -> String {
        if let Some(type_node) = self.protobuf_type_map.get(field.type_name()) {
            if let TypeSource::Enum(ref enum_proto) = type_node.source {
                if let Some(value) = enum_proto.value.iter().find(|e| e.number() == number) {
                    return value.name().to_string();
                }
            }
        }

        // Unknown values can only be written as their number
        number.to_string()
    }

    fn build_aggregate_value(
        &self,
        message_proto: &DescriptorProto,
        values: &[(u32, RawValue)],
    ) -> String {
        let mut fields: Vec<String> = Vec::new();

        for sub_field in &message_proto.field {
            for value in self.get_option_values(values, sub_field) {
                if sub_field.type_() == Type::TYPE_MESSAGE {
                    fields.push(format!("{} {value}", sub_field.name()));
                } else {
                    fields.push(format!("{}: {value}", sub_field.name()));
                }
            }
        }

        if fields.is_empty() {
            "{}".to_string()
        } else {
            format!("{{ {} }}", fields.join(" "))
        }
    }
}

// ─── Free helper functions ──────────────────────────────────────────────────

fn is_named_type(t: Type) -> bool {
    t == Type::TYPE_MESSAGE || t == Type::TYPE_ENUM
}

fn get_package_path(package: &str, name: &str) -> String {
    if name.starts_with('.') {
        return name.to_string();
    }
    let pkg = if package.is_empty() || package.starts_with('.') {
        package.to_string()
    } else {
        format!(".{package}")
    };
    format!("{pkg}.{name}")
}

fn get_label(label: Label) -> &'static str {
    match label {
        Label::LABEL_REQUIRED => "required",
        Label::LABEL_REPEATED => "repeated",
        _ => "optional",
    }
}

fn get_type(t: Type) -> &'static str {
    match t {
        Type::TYPE_INT32 => "int32",
        Type::TYPE_INT64 => "int64",
        Type::TYPE_SINT32 => "sint32",
        Type::TYPE_SINT64 => "sint64",
        Type::TYPE_UINT32 => "uint32",
        Type::TYPE_UINT64 => "uint64",
        Type::TYPE_STRING => "string",
        Type::TYPE_BOOL => "bool",
        Type::TYPE_BYTES => "bytes",
        Type::TYPE_DOUBLE => "double",
        Type::TYPE_ENUM => "enum",
        Type::TYPE_FLOAT => "float",
        Type::TYPE_GROUP => "GROUP",
        Type::TYPE_MESSAGE => "message",
        Type::TYPE_FIXED32 => "fixed32",
        Type::TYPE_FIXED64 => "fixed64",
        Type::TYPE_SFIXED32 => "sfixed32",
        Type::TYPE_SFIXED64 => "sfixed64",
    }
}

fn resolve_type(field: &FieldDescriptorProto) -> String {
    if is_named_type(field.type_()) {
        field.type_name().to_string()
    } else {
        get_type(field.type_()).to_string()
    }
}

fn bool_str(b: bool) -> String {
    if b { "true" } else { "false" }.to_string()
}

fn append_heading_space(sb: &mut String, marker: &mut bool) {
    if *marker {
        sb.push('\n');
        *marker = false;
    }
}

/// A value read straight from the wire, used to dump option values in the
/// format they were written in (the protobuf crate does not wire-format-verify
/// unknown fields).
enum RawValue {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    LengthDelimited(Vec<u8>),
}

/// Copies the unknown fields of a parsed message into a unified wire-value list.
fn raw_values_of<M: protobuf::Message>(message: &M) -> Vec<(u32, RawValue)> {
    message
        .special_fields()
        .unknown_fields()
        .iter()
        .map(|(num, val)| {
            let value = match val {
                UnknownValueRef::Varint(v) => RawValue::Varint(v),
                UnknownValueRef::Fixed32(v) => RawValue::Fixed32(v),
                UnknownValueRef::Fixed64(v) => RawValue::Fixed64(v),
                UnknownValueRef::LengthDelimited(bytes) => {
                    RawValue::LengthDelimited(bytes.to_vec())
                }
            };
            (num, value)
        })
        .collect()
}

/// Parses raw wire-format data into field-numbered wire values, without any
/// schema. Returns `None` if the data is malformed.
fn parse_wire_values(data: &[u8]) -> Option<Vec<(u32, RawValue)>> {
    let mut values = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        let (tag, new_pos) = read_varint(data, pos)?;
        if tag == 0 {
            return None;
        }
        pos = new_pos;

        let field_number = (tag >> 3) as u32;
        match tag & 0x07 {
            // Varint
            0 => {
                let (value, new_pos) = read_varint(data, pos)?;
                pos = new_pos;
                values.push((field_number, RawValue::Varint(value)));
            }
            // 64-bit
            1 => {
                let bytes = data.get(pos..pos + 8)?;
                pos += 8;
                values.push((
                    field_number,
                    RawValue::Fixed64(u64::from_le_bytes(bytes.try_into().ok()?)),
                ));
            }
            // Length-delimited
            2 => {
                let (length, new_pos) = read_varint(data, pos)?;
                let end = new_pos.checked_add(length as usize)?;
                let bytes = data.get(new_pos..end)?;
                pos = end;
                values.push((field_number, RawValue::LengthDelimited(bytes.to_vec())));
            }
            // Group: values within it are not collected, only skipped over
            3 => {
                pos = skip_group(data, pos)?;
            }
            // 32-bit
            5 => {
                let bytes = data.get(pos..pos + 4)?;
                pos += 4;
                values.push((
                    field_number,
                    RawValue::Fixed32(u32::from_le_bytes(bytes.try_into().ok()?)),
                ));
            }
            _ => return None,
        }
    }

    Some(values)
}

/// Skips over a group (wire types 3/4) in raw wire-format data.
fn skip_group(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let (tag, new_pos) = read_varint(data, pos)?;
        if tag == 0 {
            return None;
        }
        pos = new_pos;

        match tag & 0x07 {
            0 => {
                let (_, new_pos) = read_varint(data, pos)?;
                pos = new_pos;
            }
            1 => pos = pos.checked_add(8).filter(|&p| p <= data.len())?,
            2 => {
                let (length, new_pos) = read_varint(data, pos)?;
                pos = new_pos.checked_add(length as usize)?;
                if pos > data.len() {
                    return None;
                }
            }
            3 => pos = skip_group(data, pos)?,
            // End group
            4 => return Some(pos),
            5 => pos = pos.checked_add(4).filter(|&p| p <= data.len())?,
            _ => return None,
        }
    }
}

const MAX_FIELD_NUMBER: i32 = 536_870_911;

fn format_range(start: i32, end: i32, max: i32) -> String {
    if start == end {
        return format!("{start}");
    }

    if end >= max {
        format!("{start} to max")
    } else {
        format!("{start} to {end}")
    }
}

fn dump_reserved(
    ranges: impl Iterator<Item = String>,
    names: &[String],
    sb: &mut String,
    indent: &str,
    marker: &mut bool,
) {
    let range_list: Vec<String> = ranges.collect();

    if !range_list.is_empty() {
        append_heading_space(sb, marker);
        let _ = writeln!(sb, "{indent}\treserved {};", range_list.join(", "));
    }

    if !names.is_empty() {
        append_heading_space(sb, marker);
        let literals: Vec<String> = names.iter().map(|n| util::to_literal(n)).collect();
        let _ = writeln!(sb, "{indent}\treserved {};", literals.join(", "));
    }
}
