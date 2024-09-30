# frozen_string_literal: true

require 'tldr/rake'

task default: [:tldr]

task :example do
  $LOAD_PATH.unshift('lib')
  require 'wid'
  source = <<~RUBY
    puts("Hello, World!")
  RUBY

  tokens = Wid::Lexer.tokenize(source)
  puts tokens.map { _1.value.inspect }.join(' ')
  ast = Wid::Parser.parse(tokens)
  output = Wid::Codegen.generate(ast)

  File.write('example.c', output)

  system('bat example.c')
  system('gcc example.c -o example')
  system('./example')
end
