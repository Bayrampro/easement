// Resolved Dart facts only. Architecture policy belongs to the Rust evaluator.
import 'dart:convert';
import 'dart:io';

import 'package:analyzer/dart/analysis/analysis_context_collection.dart';
import 'package:analyzer/dart/analysis/results.dart';
import 'package:analyzer/dart/ast/ast.dart';
import 'package:analyzer/dart/ast/visitor.dart';
import 'package:analyzer/dart/element/element.dart';
import 'package:analyzer/dart/element/nullability_suffix.dart';
import 'package:analyzer/dart/element/type.dart';
import 'package:analyzer/source/line_info.dart';

part 'ui.dart';

const version = '1.1.0';
const formatVersion = 2;
typedef Json = Map<String, dynamic>;

Future<void> main(List<String> args) async {
  if (args.length == 1 && args.single == '--version') {
    stdout.writeln(
      'easement_dart_semantics $version facts=$formatVersion analyzer=14.4.0',
    );
    return;
  }
  try {
    final request =
        jsonDecode(await stdin.transform(utf8.decoder).join()) as Json;
    final extractor = Extractor(request['root'] as String);
    final collection = AnalysisContextCollection(
      includedPaths: [request['package'] as String],
      sdkPath: request['sdk'] as String,
    );
    final seen = <String>{};
    try {
      for (final file in <String>{
        ...(request['files'] as List).cast<String>(),
        ...((request['catalog'] as List?) ?? []).cast<String>(),
      }) {
        final path = '${extractor.root}/$file';
        final context =
            collection.contexts
                .where((c) => c.contextRoot.isAnalyzed(path))
                .firstOrNull ??
            collection.contexts.first;
        final result = await context.currentSession
            .getResolvedLibraryContaining(path);
        if (result is! ResolvedLibraryResult) {
          extractor.gaps.add({
            'file': file,
            'line': 1,
            'reason': 'library resolution returned ${result.runtimeType}',
          });
          continue;
        }
        for (final unit in result.units) {
          if (!unit.path.startsWith('${extractor.root}/') ||
              !seen.add(unit.path))
            continue;
          extractor.units[unit.path] = unit;
          unit.unit.accept(Inventory(extractor));
          for (final diagnostic in unit.diagnostics) {
            if (diagnostic.severity.name != 'error') continue;
            extractor.gaps.add({
              ...extractor.location(unit.path, diagnostic.offset),
              'reason':
                  '${diagnostic.diagnosticCode.lowerCaseName}: ${diagnostic.message}',
            });
          }
        }
      }
      for (final entry in extractor.elements.entries) {
        extractor.finish(entry.key, entry.value);
      }
      final ui = UiInventory(extractor);
      for (final unit in extractor.units.values) {
        unit.unit.accept(ui);
      }
      stdout.writeln(
        jsonEncode({
          'format_version': formatVersion,
          'extractor_version': version,
          'analyzer_version': '14.4.0',
          'sdk_version': Platform.version.split(' ').first,
          'files':
              extractor.units.keys
                  .map((p) => p.substring(extractor.root.length + 1))
                  .toList()
                ..sort(),
          'declarations': extractor.declarations.values.toList(),
          'gaps': extractor.gaps,
          'ui_classes': ui.classes,
          'callables': ui.callables,
        }),
      );
    } finally {
      await collection.dispose();
    }
  } catch (error, trace) {
    stderr.writeln('Dart semantic extraction failed: $error\n$trace');
    exitCode = 1;
  }
}

class Extractor {
  final String root;
  final units = <String, ResolvedUnitResult>{};
  final declarations = <String, Json>{};
  final elements = <Element, Json>{};
  final nodes = <Element, AstNode>{};
  final gaps = <Json>[];
  Extractor(this.root);

  Json identity(Element element) => {
    'library': element.library?.uri.toString() ?? '',
    'name': element.name ?? '',
  };
  String key(Element element) => '${element.library?.uri}#${element.name}';
  Json location(String path, int offset) {
    final line =
        units[path]?.lineInfo ??
        LineInfo.fromContent(File(path).readAsStringSync());
    final position = line.getLocation(offset);
    return {
      'file': path.startsWith('$root/') ? path.substring(root.length + 1) : '',
      'line': position.lineNumber,
      'column': position.columnNumber,
    };
  }

  Json elementLocation(Element element) {
    final fragment = element.baseElement.firstFragment;
    final path = fragment.libraryFragment?.source.fullName;
    return path == null
        ? {'file': '', 'line': 1, 'column': 1}
        : location(path, fragment.nameOffset ?? fragment.offset);
  }

  Json type(
    DartType value, [
    Set<(DartType, TypeAliasElement?, bool)>? active,
    bool expandAlias = true,
  ]) {
    active ??= <(DartType, TypeAliasElement?, bool)>{};
    final alias = expandAlias ? value.alias : null;
    final display = value.getDisplayString();
    final id = (value, alias?.element, expandAlias);
    final result = <String, dynamic>{
      'display': display,
      'nullable': value.nullabilitySuffix == NullabilitySuffix.question,
      'children': <Json>[],
    };
    if (value is InterfaceType) {
      result.addAll(identity(value.element));
      result['origin'] = elementLocation(value.element);
    }
    if (value is TypeParameterType) result['parameter'] = value.element.name;
    if (value is InvalidType) result['unresolved'] = true;
    if (alias != null) {
      result.addAll(identity(alias.element));
      result['display'] =
          '${alias.element.name}${alias.typeArguments.isEmpty ? '' : '<${alias.typeArguments.map((t) => t.getDisplayString()).join(', ')}>'}';
      result['alias'] = true;
      result['origin'] = elementLocation(alias.element);
    }
    if (!active.add(id)) return result;
    final children = result['children'] as List<Json>;
    if (alias != null) {
      children.add(type(value, active, false));
    } else if (value is InterfaceType) {
      children.addAll(value.typeArguments.map((t) => type(t, active)));
    } else if (value is FunctionType) {
      children.add(type(value.returnType, active));
      children.addAll(value.formalParameters.map((p) => type(p.type, active)));
      for (final parameter in value.typeParameters) {
        if (parameter.bound case final bound?)
          children.add(type(bound, active));
      }
    } else if (value is RecordType) {
      children.addAll(value.positionalFields.map((f) => type(f.type, active)));
      children.addAll(value.namedFields.map((f) => type(f.type, active)));
    } else if (value is TypeParameterType && value.element.bound != null) {
      children.add(type(value.element.bound!, active));
    }
    active.remove(id);
    return result;
  }

  Json signatureLocation(AstNode? annotation, Json fallback) {
    if (annotation == null) return fallback;
    final finder = AliasLocation(this);
    annotation.accept(finder);
    return finder.location ?? finder.first ?? fallback;
  }

  AstNode? returnAnnotation(AstNode? node) {
    if (node is MethodDeclaration) return node.returnType;
    if (node is FunctionDeclaration) return node.returnType;
    if (node is VariableDeclaration && node.parent is VariableDeclarationList) {
      return (node.parent as VariableDeclarationList).type;
    }
    return null;
  }

  void finish(Element element, Json declaration) {
    final api = declaration['api'] as List<Json>;
    final members = <ExecutableElement>[];
    if (element is InterfaceElement) {
      members.addAll(element.thisType.interfaceMembers.values);
      members.addAll(element.methods.where((m) => m.isStatic));
      members.addAll(element.getters.where((m) => m.isStatic));
      members.addAll(element.setters.where((m) => m.isStatic));
      members.addAll(element.constructors.where((m) => m.isOriginDeclaration));
      declaration['supertypes'] = element.thisType.allSupertypes
          .map((t) => type(t))
          .toList();
      for (final parameter in element.typeParameters) {
        if (parameter.bound case final bound?) {
          api.add({
            'member': element.name ?? '',
            'operation': 'bound:${parameter.name}',
            'location': signatureLocation(
              nodes[parameter.baseElement],
              elementLocation(parameter),
            ),
            'origin': elementLocation(parameter),
            'type': type(bound),
          });
        }
      }
    } else if (element is ExecutableElement) {
      members.add(element);
    }
    final seen = <String>{};
    for (final member in members) {
      // A field's generated setter repeats the type already exposed by its
      // generated getter. Explicit setter declarations remain separate API slots.
      if (member is SetterElement && member.isOriginVariable) continue;
      if (!member.isPublic || !seen.add('${member.kind}:${member.name}'))
        continue;
      final origin = elementLocation(member);
      final inherited = member.baseElement.enclosingElement != element;
      final node =
          nodes[member.baseElement] ??
          (member is PropertyAccessorElement
              ? nodes[member.variable.baseElement]
              : null);
      final fallback = inherited || origin['file'] == ''
          ? declaration['location'] as Json
          : origin;
      void add(DartType value, String operation, AstNode? annotation, Json at) {
        api.add({
          'member': member.name ?? '',
          'operation': operation,
          'location': inherited
              ? declaration['location']
              : signatureLocation(annotation, at),
          'origin': origin,
          'type': type(value),
        });
      }

      if (member is! ConstructorElement)
        add(member.returnType, 'return', returnAnnotation(node), fallback);
      for (final parameter in member.formalParameters) {
        final parameterAt = elementLocation(parameter);
        add(
          parameter.type,
          'parameter:${parameter.name}',
          nodes[parameter.baseElement] ?? returnAnnotation(node),
          parameterAt['file'] == '' ? fallback : parameterAt,
        );
      }
      for (final parameter in member.typeParameters) {
        if (parameter.bound case final bound?)
          add(
            bound,
            'bound:${parameter.name}',
            nodes[parameter.baseElement],
            fallback,
          );
      }
    }
  }
}

class Inventory extends UnifyingAstVisitor<void> {
  final Extractor extractor;
  Json? owner;
  Inventory(this.extractor);

  @override
  void visitNode(AstNode node) {
    final previous = owner;
    final fragment = node is FragmentDeclaringNode
        ? node.declaredFragment
        : null;
    final element = fragment?.element;
    if (element != null) extractor.nodes[element.baseElement] = node;
    if (element is InterfaceElement ||
        element is TypeAliasElement ||
        element is TopLevelFunctionElement ||
        element is TopLevelVariableElement) {
      final resolved = element!;
      final modifiers = <String>[];
      if (resolved is ClassElement) {
        if (resolved.isAbstract) modifiers.add('abstract');
        if (resolved.isInterface) modifiers.add('interface');
        if (resolved.isBase) modifiers.add('base');
        if (resolved.isFinal) modifiers.add('final');
        if (resolved.isSealed) modifiers.add('sealed');
      }
      owner = {
        'identity': extractor.identity(resolved),
        'kind': resolved.kind.name,
        'location': extractor.elementLocation(resolved),
        'modifiers': modifiers,
        'api': <Json>[],
        'uses': <Json>[],
        'supertypes': <Json>[],
      };
      extractor.declarations[extractor.key(resolved)] = owner!;
      extractor.elements[resolved] = owner!;
    }
    if (owner != null) {
      DartType? resolved;
      var operation = 'reference';
      if (node is NamedType) {
        resolved = node.type;
        if (node.thisOrAncestorOfType<ExtendsClause>() != null ||
            node.thisOrAncestorOfType<ImplementsClause>() != null ||
            node.thisOrAncestorOfType<WithClause>() != null)
          operation = 'inheritance';
      }
      if (node is Expression && node is! SimpleIdentifier)
        resolved = node.staticType;
      if (node is SimpleIdentifier && !node.inDeclarationContext()) {
        resolved = node.staticType;
        final target = node.element;
        if (target is InterfaceElement) resolved = target.thisType;
        if (target is ExecutableElement && resolved == null)
          resolved = target.type;
      }
      if (element is VariableElement) {
        resolved = element.type;
        operation = 'variable';
      }
      if (node is InstanceCreationExpression) operation = 'construct';
      if (resolved != null) {
        final unit = node.thisOrAncestorOfType<CompilationUnit>();
        final path = unit?.declaredFragment?.source.fullName;
        if (path != null) {
          (owner!['uses'] as List<Json>).add({
            'member': owner!['identity']['name'],
            'operation': operation,
            'location': extractor.location(path, node.offset),
            'origin': extractor.location(path, node.offset),
            'type': extractor.type(resolved),
          });
        }
      }
    }
    node.visitChildren(this);
    owner = previous;
  }
}

class AliasLocation extends RecursiveAstVisitor<void> {
  final Extractor extractor;
  Json? location;
  Json? first;
  AliasLocation(this.extractor);
  @override
  void visitBlockFunctionBody(BlockFunctionBody node) {}

  @override
  void visitExpressionFunctionBody(ExpressionFunctionBody node) {}

  @override
  void visitNamedType(NamedType node) {
    final path = node
        .thisOrAncestorOfType<CompilationUnit>()
        ?.declaredFragment
        ?.source
        .fullName;
    if (path != null) first ??= extractor.location(path, node.offset);
    if (node.type?.alias != null && path != null)
      location ??= extractor.location(path, node.offset);
    super.visitNamedType(node);
  }
}
