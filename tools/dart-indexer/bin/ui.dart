part of 'semantics.dart';

const flutterFramework = 'package:flutter/src/widgets/framework.dart';

bool frameworkType(InterfaceElement element, String name) =>
    element.library.uri.toString() == flutterFramework && element.name == name;

/// Null means resolution cannot prove either outcome. Never parse type spelling.
bool? widgetType(DartType? value, [Set<DartType>? seen]) {
  if (value == null || value is InvalidType || value is DynamicType)
    return null;
  seen ??= <DartType>{};
  if (!seen.add(value)) return null;
  if (value is TypeParameterType) return widgetType(value.element.bound, seen);
  if (value is InterfaceType) {
    if (frameworkType(value.element, 'Widget') ||
        value.allSupertypes.any((t) => frameworkType(t.element, 'Widget'))) {
      return true;
    }
    if (value.isDartCoreObject) return null;
  }
  return false;
}

class UiInventory extends RecursiveAstVisitor<void> {
  final Extractor extractor;
  final classes = <Json>[];
  final callables = <Json>[];
  final localCounts = <String, int>{};
  final callableIds = <ExecutableElement, String>{};
  String? lexicalOwner;
  UiInventory(this.extractor);

  @override
  void visitClassDeclaration(ClassDeclaration node) {
    final element = node.declaredFragment?.element;
    if (element == null) {
      missing(node, 'class declaration could not be resolved');
    } else {
      classes.add({
        'identity': extractor.identity(element),
        'location': extractor.elementLocation(element),
        'widget': widgetType(element.thisType),
      });
    }
    final previous = lexicalOwner;
    lexicalOwner = element?.name;
    super.visitClassDeclaration(node);
    lexicalOwner = previous;
  }

  void missing(AstNode node, String reason) {
    final path = node
        .thisOrAncestorOfType<CompilationUnit>()
        ?.declaredFragment
        ?.source
        .fullName;
    if (path != null)
      extractor.gaps.add({
        ...extractor.location(path, node.offset),
        'reason': reason,
      });
  }

  @override
  void visitConstructorDeclaration(ConstructorDeclaration node) {
    final previous = lexicalOwner;
    lexicalOwner =
        '${lexicalOwner ?? ownerName(node.declaredFragment?.element.enclosingElement)}::${node.name?.lexeme ?? 'new'}';
    super.visitConstructorDeclaration(node);
    lexicalOwner = previous;
  }

  String ownerName(Element? element) {
    if (element == null || element is LibraryElement) return '';
    if (element is ExecutableElement && callableIds.containsKey(element)) {
      return callableIds[element]!;
    }
    final parent = ownerName(element.enclosingElement);
    return [
      parent,
      element.name ?? '<anonymous>',
    ].where((s) => s.isNotEmpty).join('::');
  }

  List<String> overrides(ExecutableElement element, Set<Element> seen) {
    if (element is! MethodElement ||
        element.isStatic ||
        !seen.add(element.baseElement))
      return [];
    final owner = element.enclosingElement;
    if (owner is! InterfaceElement) return [];
    final result = <String>[];
    for (final target
        in owner.getOverridden(
              Name.forLibrary(element.library, element.name!),
            ) ??
            <ExecutableElement>[]) {
      final parent = target.enclosingElement;
      result.add('${target.library.uri}#${parent?.name}::${target.name}');
      result.addAll(overrides(target, seen));
    }
    return result;
  }

  String? callable(ExecutableElement? element, FunctionBody body, String kind) {
    if (element == null) return null;
    final enclosing = lexicalOwner ?? ownerName(element.enclosingElement);
    var id = [
      enclosing,
      element.name ?? '<anonymous>',
    ].where((s) => s.isNotEmpty).join('::');
    if (element is LocalFunctionElement) {
      final count = localCounts.update(
        '${element.library.uri}#$id',
        (n) => n + 1,
        ifAbsent: () => 1,
      );
      id = '$id[local:$count]';
    }
    callableIds[element] = id;
    final returns = UiReturns(extractor);
    body.accept(returns);
    final declared = widgetType(element.returnType);
    bool? classification = declared;
    if (body.isAsynchronous || body.isGenerator) {
      classification = false;
    } else if (classification == null) {
      final evidence = returns.kinds;
      if (evidence.contains('widget') &&
          evidence.every((v) => v == 'widget' || v == 'null') &&
          (body is ExpressionFunctionBody ||
              (body is BlockFunctionBody && terminates(body.block)))) {
        classification = true;
      } else if (evidence.isNotEmpty &&
          evidence.every((v) => v == 'other' || v == 'null')) {
        classification = false;
      }
    }
    final targets = overrides(element, <Element>{});
    final frameworkBuild = targets.any(
      (target) =>
          target == '$flutterFramework#StatelessWidget::build' ||
          target == '$flutterFramework#State::build' ||
          target ==
              'package:flutter_riverpod/src/core.dart#ConsumerWidget::build',
    );
    callables.add({
      'id': '${element.library.uri}#$id',
      'name': element.name,
      'kind': kind,
      'enclosing': enclosing.isEmpty ? null : enclosing,
      'location': extractor.elementLocation(element),
      'widget_return': classification,
      'framework_build': frameworkBuild,
      'override_targets': targets,
      'return_type': extractor.type(element.returnType),
      'returns': returns.values,
    });
    return id;
  }

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    final previous = lexicalOwner;
    lexicalOwner = callable(
      node.declaredFragment?.element,
      node.body,
      node.isGetter
          ? 'getter'
          : node.isSetter
          ? 'setter'
          : node.isStatic
          ? 'static-method'
          : 'method',
    );
    super.visitMethodDeclaration(node);
    lexicalOwner = previous;
  }

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    final element =
        node.declaredFragment?.element ??
        node.functionExpression.declaredFragment?.element;
    if (element == null) missing(node, 'named function could not be resolved');
    final previous = lexicalOwner;
    lexicalOwner = callable(
      element,
      node.functionExpression.body,
      node.isGetter
          ? 'getter'
          : element is LocalFunctionElement
          ? 'local-function'
          : 'function',
    );
    super.visitFunctionDeclaration(node);
    lexicalOwner = previous;
  }
}

/// Conservative fallthrough check; unsupported control flow stays unknown.
bool terminates(Statement statement) {
  if (statement is ReturnStatement) return true;
  if (statement is ExpressionStatement &&
      statement.expression is ThrowExpression)
    return true;
  if (statement is Block) return statement.statements.any(terminates);
  if (statement is IfStatement) {
    final other = statement.elseStatement;
    return other != null &&
        terminates(statement.thenStatement) &&
        terminates(other);
  }
  return false;
}

class UiReturns extends RecursiveAstVisitor<void> {
  final Extractor extractor;
  final values = <Json>[];
  final kinds = <String>[];
  UiReturns(this.extractor);

  void expression(Expression? value) {
    if (value is ParenthesizedExpression) {
      expression(value.expression);
      return;
    }
    if (value is ConditionalExpression) {
      expression(value.thenExpression);
      expression(value.elseExpression);
      return;
    }
    if (value is ThrowExpression) return;
    if (value == null || value is NullLiteral) {
      kinds.add('null');
      return;
    }
    final type = value.staticType;
    final widget = widgetType(type);
    kinds.add(
      widget == true
          ? 'widget'
          : widget == false
          ? 'other'
          : 'unknown',
    );
    final path = value
        .thisOrAncestorOfType<CompilationUnit>()
        ?.declaredFragment
        ?.source
        .fullName;
    if (type != null && path != null) {
      final location = extractor.location(path, value.offset);
      values.add({
        'member': '<return>',
        'operation': 'return-expression',
        'location': location,
        'origin': location,
        'type': extractor.type(type),
      });
    }
  }

  @override
  void visitReturnStatement(ReturnStatement node) =>
      expression(node.expression);
  @override
  void visitExpressionFunctionBody(ExpressionFunctionBody node) =>
      expression(node.expression);
  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {}
  @override
  void visitFunctionExpression(FunctionExpression node) {}
}
