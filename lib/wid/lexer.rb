# frozen_string_literal: true

require "strscan"

module Wid
  class Lexer
    Token = Data.define(:type, :value, :line, :column)

    IDENTIFIER = /[_A-Za-z][_0-9A-Za-z]*\b/
    WHITESPACE = /[ \r\t]+/
    INTEGER = /-?(?:0|[1-9][0-9]*)/
    FLOAT_DECIMAL = /[.][0-9]+/
    NUMBER = /#{INTEGER}(#{FLOAT_DECIMAL})?/
    STRING = /"([^"\\]|\\.)*"/
    # STRING =/"[^"]*"/

    KEYWORDS = %w[true false nil def end].freeze

    KW_RE = /#{Regexp.union(KEYWORDS.sort)}\b/
    KW_TABLE = KEYWORDS.map { [_1, _1.upcase.to_sym] }.to_h

    LITERALS = %W[{ } ( ) [ ] = ! | & + - , \n].freeze
    DOT = "."

    PUNCTUATION = Regexp.union(LITERALS)
    PUNCTUATION_TABLE = LITERALS.map { |x| [x, x.to_sym] }.to_h

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

      while (tok = next_token)
        tokens << tok
      end

      tokens << token(:EOF)
    end

    def column_number
      @scanner.pos - @last_pos - @scanner.matched_size.to_i
    end

    def next_token
      @scanner.skip(WHITESPACE)

      return if @scanner.eos?

      tok = if @scanner.scan(PUNCTUATION)
        token(PUNCTUATION_TABLE[@scanner.matched], @scanner.matched)
      elsif @scanner.scan(KW_RE)
        token(KW_TABLE[@scanner.matched], @scanner.matched)
      elsif @scanner.scan(STRING)
        token(:STRING, @scanner.matched)
      elsif @scanner.scan(IDENTIFIER)
        token(:IDENTIFIER, @scanner.matched)
      elsif @scanner.scan(NUMBER)
        token(:NUMBER, @scanner.matched)
      elsif @scanner.scan(DOT)
        token(:DOT)
      else
        # print out the lines around the error line to give context
        ch = @scanner.getch

        warn "warn: Syntax error at #{@line_number + 1}:#{column_number}: unknown token `#{ch}'"
        lines = @scanner.string.split("\n")
        lines[[@line_number - 1, 0].max..@line_number + 1].each_with_index do |line, i|
          lineno = @line_number + i
          line_prefix = " #{lineno.to_s.ljust(lines.size - 1)} | "
          warn "#{line_prefix}#{line}"
          if @line_number == lineno - 1
            warn " " * (column_number + line_prefix.size) + "^"
          end
        end

        token(:UNKNOWN, ch)
      end

      if @scanner.matched == "\n"
        @last_pos = @scanner.pos
        @line_number += 1
      end

      tok
    end

    private

    def token(type, value = nil)
      Token.new(type: type, value: value, line: @line_number, column: column_number)
    end
  end
end
