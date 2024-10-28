# frozen_string_literal: true

require "zeitwerk"

loader = Zeitwerk::Loader.for_gem
loader.inflector.inflect("ast" => "AST")
loader.setup

module Wid
  def self.parse(input)
    tokens = Lexer.tokenize(input)
    Parser.parse(tokens)
  rescue Parser::UnexpectedTokenError => e
    message = [e.message]
    # TODO: Fix the line numbers being incorrect

    token_line = e.token.line
    input.each_line.each_with_index do |line, index|
      line_number = index + 1
      if line_number.between?(token_line - 3, token_line + 3)
        formatted_line = line_number.to_s.rjust(4)
        message << "#{formatted_line}: #{line.chomp}"
        if line_number == e.line + 1
          message << " " * (e.column + formatted_line.size + 1) + "^"
        end
      end
    end

    raise message.join("\n")
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
