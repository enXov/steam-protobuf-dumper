use crate::util;
use protobuf::descriptor::field_descriptor_proto::{Label, Type};
use protobuf::descriptor::{
    DescriptorProto, EnumDescriptorProto, EnumOptions, EnumValueOptions, FieldDescriptorProto,
    FieldOptions, FileDescriptorProto, FileOptions, MessageOptions, MethodOptions,
    ServiceDescriptorProto, ServiceOptions,
};
use protobuf::Message;
use protobuf::UnknownValueRef;
use std::collections::{HashMap, HashSet};
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
}

impl ProtobufDumper {
    pub fn new(protobufs: Vec<FileDescriptorProto>) -> Self {
        Self {
            protobufs,
            protobuf_map: HashMap::new(),
            protobuf_type_map: HashMap::new(),
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
                set.insert(dep.to_string());
                self.recursive_add_public_dependencies(set, dep, depth + 1);
            }
        } else {
            for &idx in &proto.public_dependency {
                if let Some(dep) = proto.dependency.get(idx as usize) {
                    set.insert(dep.to_string());
                    self.recursive_add_public_dependencies(set, dep, depth + 1);
                }
            }
        }
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
    }

    fn dump_extension_descriptors(
        &self,
        source: &FileDescriptorProto,
        fields: &[FieldDescriptorProto],
        sb: &mut String,
        level: usize,
        marker: &mut bool,
    ) {
        // Group by extendee
        let mut groups: HashMap<&str, Vec<&FieldDescriptorProto>> = HashMap::new();
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

        // Nested messages
        for nested in &proto.nested_type {
            self.dump_descriptor(source, nested, sb, level + 1, &mut inner_marker);
        }

        // Nested enums
        for enum_type in &proto.enum_type {
            self.dump_enum_descriptor(source, enum_type, sb, level + 1, &mut inner_marker);
        }

        // Root fields (not part of a oneof)
        let root_fields: Vec<&FieldDescriptorProto> = proto
            .field
            .iter()
            .filter(|f| !f.has_oneof_index())
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

            append_heading_space(sb, &mut inner_marker);
            let _ = writeln!(sb, "{indent}\toneof {} {{", oneof.name());

            for field in &oneof_fields {
                let decl = self.build_descriptor_declaration(source, field, false);
                let _ = writeln!(sb, "{indent}\t\t{decl}");
            }

            let _ = writeln!(sb, "{indent}\t}}");
            inner_marker = true;
        }

        // Extension ranges
        for range in &proto.extension_range {
            let max = if range.end() >= 536_870_911 {
                "max".to_string()
            } else {
                range.end().to_string()
            };

            append_heading_space(sb, &mut inner_marker);
            let _ = writeln!(sb, "{indent}\textensions {} to {max};", range.start());
        }

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
        let type_name = resolve_type(field);
        let mut options: Vec<(String, String)> = Vec::new();

        // Default value
        if !field.default_value().is_empty() {
            let default = if field.type_() == Type::TYPE_STRING {
                util::to_literal(field.default_value())
            } else {
                field.default_value().to_string()
            };
            options.push(("default".to_string(), default));
        } else if field.type_() == Type::TYPE_ENUM && field.label() != Label::LABEL_REPEATED {
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

        let mut decl = String::new();
        if emit_field_label {
            let _ = write!(decl, "{} ", get_label(field.label()));
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

        self.dump_options_extensions(source, ".google.protobuf.MethodOptions", opts, &mut kv);
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

        // Find all extension fields that extend this options type
        for (type_path, type_node) in &self.protobuf_type_map {
            let proto_name = match &type_node.proto_name {
                Some(n) => n,
                None => continue,
            };

            if !dependencies.contains(proto_name) {
                continue;
            }

            if let TypeSource::Field(ref field) = type_node.source {
                if !field.extendee().is_empty() && field.extendee() == type_name {
                    self.dump_options_field_recursive(field, options, kv, None, type_path);
                }
            }
        }
    }

    fn dump_options_field_recursive<M: protobuf::Message>(
        &self,
        field: &FieldDescriptorProto,
        message: &M,
        kv: &mut Vec<(String, String)>,
        path: Option<&str>,
        _type_path: &str,
    ) {
        let key = match path {
            Some(p) => format!("{}.{}", p, field.name()),
            None => format!("({})", field.name()),
        };

        let unknown_fields = message.special_fields().unknown_fields();

        if is_named_type(field.type_()) && !field.type_name().is_empty() {
            if let Some(type_node) = self.protobuf_type_map.get(field.type_name()) {
                match &type_node.source {
                    TypeSource::Enum(enum_proto) => {
                        // Try to read as varint from unknown fields
                        for val in unknown_fields.iter() {
                            if val.0 == field.number() as u32 {
                                if let UnknownValueRef::Varint(v) = val.1 {
                                    if let Some(ev) =
                                        enum_proto.value.iter().find(|e| e.number() == v as i32)
                                    {
                                        kv.push((key.clone(), ev.name().to_string()));
                                    }
                                }
                            }
                        }
                    }
                    TypeSource::Message(msg_proto) => {
                        // Try to read as length-delimited from unknown fields
                        for val in unknown_fields.iter() {
                            if val.0 == field.number() as u32 {
                                if let UnknownValueRef::LengthDelimited(bytes) = val.1 {
                                    // Parse as a generic message to get its unknown fields
                                    if let Ok(sub_msg) =
                                        protobuf::descriptor::DescriptorProto::parse_from_bytes(
                                            bytes,
                                        )
                                    {
                                        for sub_field in &msg_proto.field {
                                            self.dump_options_field_recursive(
                                                sub_field,
                                                &sub_msg,
                                                kv,
                                                Some(&key),
                                                _type_path,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            // Scalar type — extract from unknown fields
            if let Some(value) = extract_scalar_from_unknown(unknown_fields, field) {
                kv.push((key, value));
            }
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

/// Extracts a scalar value from the unknown fields of a message, matching by field number.
fn extract_scalar_from_unknown(
    unknown_fields: &protobuf::UnknownFields,
    field: &FieldDescriptorProto,
) -> Option<String> {
    let field_num = field.number() as u32;

    for (num, val) in unknown_fields.iter() {
        if num != field_num {
            continue;
        }
        match field.type_() {
            Type::TYPE_INT32 | Type::TYPE_UINT32 | Type::TYPE_FIXED32 => {
                if let UnknownValueRef::Varint(v) = val {
                    return Some((v as u32).to_string());
                }
                if let UnknownValueRef::Fixed32(v) = val {
                    return Some(v.to_string());
                }
            }
            Type::TYPE_INT64 | Type::TYPE_UINT64 | Type::TYPE_FIXED64 => {
                if let UnknownValueRef::Varint(v) = val {
                    return Some(v.to_string());
                }
                if let UnknownValueRef::Fixed64(v) = val {
                    return Some(v.to_string());
                }
            }
            Type::TYPE_SINT32 | Type::TYPE_SFIXED32 => {
                if let UnknownValueRef::Varint(v) = val {
                    return Some((v as i32).to_string());
                }
                if let UnknownValueRef::Fixed32(v) = val {
                    return Some((v as i32).to_string());
                }
            }
            Type::TYPE_SINT64 | Type::TYPE_SFIXED64 => {
                if let UnknownValueRef::Varint(v) = val {
                    return Some((v as i64).to_string());
                }
                if let UnknownValueRef::Fixed64(v) = val {
                    return Some((v as i64).to_string());
                }
            }
            Type::TYPE_STRING => {
                if let UnknownValueRef::LengthDelimited(bytes) = val {
                    if let Ok(s) = std::str::from_utf8(bytes) {
                        return Some(util::to_literal(s));
                    }
                }
            }
            Type::TYPE_BOOL => {
                if let UnknownValueRef::Varint(v) = val {
                    return Some(if v != 0 { "true" } else { "false" }.to_string());
                }
            }
            Type::TYPE_BYTES => {
                if let UnknownValueRef::LengthDelimited(bytes) = val {
                    return Some(format!("{:?}", bytes));
                }
            }
            Type::TYPE_DOUBLE => {
                if let UnknownValueRef::Fixed64(v) = val {
                    return Some(f64::from_bits(v).to_string());
                }
            }
            Type::TYPE_FLOAT => {
                if let UnknownValueRef::Fixed32(v) = val {
                    return Some(f32::from_bits(v).to_string());
                }
            }
            _ => {}
        }
    }

    None
}
