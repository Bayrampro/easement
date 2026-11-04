// Real Flutter identities and override evidence, extracted without policy.
import 'dart:convert';
import 'dart:io';

void expect(bool condition, String reason) {
  if (!condition) throw StateError(reason);
}

Future<void> main() async {
  final package = File.fromUri(Platform.script.resolve('../pubspec.yaml'))
      .parent;
  final root = package.uri
      .resolve('../../tests/fixtures/dart/flutter-ui/')
      .toFilePath();
  final process = await Process.start(Platform.resolvedExecutable, [
    '--packages=${package.path}/.dart_tool/package_config.json',
    '${package.path}/bin/semantics.dart',
  ]);
  process.stdin.write(
    jsonEncode({
      'root': Directory(root).path.replaceFirst(RegExp(r'/$'), ''),
      'package': Directory(root).path.replaceFirst(RegExp(r'/$'), ''),
      'sdk': File(Platform.resolvedExecutable).parent.parent.path,
      'files': ['lib/profile_screen.dart'],
    }),
  );
  await process.stdin.close();
  final outputs = await Future.wait([
    process.stdout.transform(utf8.decoder).join(),
    process.stderr.transform(utf8.decoder).join(),
  ]);
  expect(await process.exitCode == 0, outputs[1]);
  final facts = jsonDecode(outputs[0]) as Map<String, dynamic>;
  expect(facts['format_version'] == 2, 'UI fact format');
  expect((facts['gaps'] as List).isEmpty, 'real Flutter fixture has gaps');
  final classes = (facts['ui_classes'] as List).cast<Map<String, dynamic>>();
  expect(
    classes.length == 2 && classes.every((c) => c['widget'] == true),
    'private widget ancestry missing',
  );
  final callables = (facts['callables'] as List).cast<Map<String, dynamic>>();
  final helper = callables.singleWhere((c) => c['name'] == '_header');
  expect(
    helper['location']['line'] == 9 && helper['widget_return'] == true,
    'private helper location/type missing',
  );
  expect(
    helper['framework_build'] == false,
    'helper mistaken for framework build',
  );
  expect(
    (helper['returns'] as List).single['type']['name'] == '_ProfileHeader',
    'return expression identity missing',
  );
  for (final build in callables.where((c) => c['name'] == 'build')) {
    expect(build['framework_build'] == true, 'framework override missing');
    expect(
      (build['override_targets'] as List).contains(
        'package:flutter/src/widgets/framework.dart#StatelessWidget::build',
      ),
      'resolved override target missing',
    );
  }
  stdout.writeln('Flutter UI extraction tests passed');
}
