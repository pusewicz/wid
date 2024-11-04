# frozen_string_literal: true

require "zeitwerk"

loader = Zeitwerk::Loader.for_gem
loader.inflector.inflect("ast" => "AST")
loader.setup

module Wid
  def self.parse(input)
    tokens = Tokenizer.tokenize(input)
    Parser.parse(tokens)
  end

  def self.generate(input, debug: false)
    ast = parse(input)
    dump_ast(ast) if debug
    Codegen.generate(ast)
  end

  def self.dump_ast(ast)
    ast.children.each do |child|
      puts child.inspect
    end
  end
end
