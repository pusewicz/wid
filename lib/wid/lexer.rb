# frozen_string_literal: true

require_relative "lexer/errors"
require_relative "lexer/token"

require "strscan"

module Wid
  class Lexer
    SPEC = [
      # Whitespace
      [/[ \r\t]+/, nil],

      # Comments
      [/#.*[\n]?/, nil],

      # New line
      [/[\n]/, :NEW_LINE],

      # Symbols
      [/;/, :";"],
      [/\{/, :"{"],
      [/\}/, :"}"],
      [/\(/, :"("],
      [/\)/, :")"],
      [/,/, :","],
      [/\./, :"."],
      [/\[/, :"["],
      [/\]/, :"]"],

      # Keywords
      [/true\b/, :true],
      [/false\b/, :false],
      [/nil\b/, :nil],

      # Numbers
      [/\d+(\.\d+)?/, :NUMBER],

      # Identifiers
      [/[_A-Za-z][_0-9A-Za-z]*\b/, :IDENTIFIER],

      # Equality operators: ==, !=
      [/[=!]=/, :EQUALITY_OPERATOR],

      # Assignment operators: =, *=, /=, +=, -=
      [/=/, :SIMPLE_ASSIGN],
      [/[\*\/\+\-]=/, :COMPLEX_ASSIGN],

      # Math operators: +, -, *, /
      [/[+\-]/, :ADDITIVE_OPERATOR],
      [/[*\/]/, :MULTIPLICATIVE_OPERATOR],

      # Logical operators
      [/^&&/, :LOGICAL_AND],
      [/^\|\|/, :LOGICAL_OR],
      [/^!/, :LOGICAL_NOT],

      # Strings: double and single-quoted
      [/"[^"]*"|'[^']*'/, :STRING]
    ].freeze

    def self.tokenize(input)
      new(input).tokenize
    end

    def initialize(input)
      @scanner = StringScanner.new(input)
      @last_pos = 0
      @line_number = 0
    end

    def tokenize
      tokens = []

      while (token = next_token)
        tokens << token
      end

      tokens.freeze
    end

    def next_token
      SPEC.each do |(pattern, type)|
        if (match = @scanner.scan(pattern))
          case type
          when nil then next
          when :NEW_LINE
            return token(match.to_sym, match).tap do
              @last_pos = @scanner.pos
              @line_number += 1
            end
          else return token(type, match)
          end
        end
      end

      # FIXME: We might be skipping the last unknow token
      return if @scanner.eos?

      raise SyntaxError.new("Unknown token #{@scanner.getch.inspect}", @line_number + 1, column_number)
    end

    private

    def token(type, value = nil)
      Token.new(type: type, value: value, line: @line_number, column: column_number)
    end

    def column_number
      @scanner.pos - @last_pos - @scanner.matched_size.to_i
    end
  end
end
