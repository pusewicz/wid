# frozen_string_literal: true

require_relative "wid/version"
require_relative "wid/parser"
require_relative "wid/lexer"
require_relative "wid/codegen"

module Wid
  def self.parse(input)
    tokens = Lexer.tokenize(input)
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
