# frozen_string_literal: true

require "wid"

module Wid
  class Test < TLDR
  end
end

module Wid
  class ParserTest < Test
    private

    def assert_ast_equal(expected, actual)
      assert_equal(expected, actual.to_hash)
    end

    def parse(input)
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

      fail message.join("\n")
    end
  end
end
