# frozen_string_literal: true

module Wid
  class Parser
    class StandardError < ::StandardError; end

    class ParseError < StandardError; end

    def initialize(tokens)
      @tokens = tokens
      @current = 0
    end

    def self.parse(tokens)
      new(tokens).parse
    end

    def parse
      statements = []

      statements << statement until eof?
      statements
    end

    # Statement → ExpressionStmt | PrintStmt
    def statement
      if match?(:PRINT)
        print_statement
      else
        expression
      end
    end

    # print_statement → "print" expression ( "," expression )* ;
    def print_statement
      expressions = []

      # Parse first expression (required)
      expressions << expression

      # Parse additional expressions separated by commas
      expressions << expression while match?(:comma)

      # Expect newline or semicolon after print statement
      consume(:"\n", "Expect newline after print statement") unless match?(:";")

      AST::Stmt::Print.new(expressions:)
    end

    # expression → equality ;
    def expression
      # assignment
      primary
    end

    # equality → comparison ( ( "!=" | "==" ) comparison )* ;
    def equality
    end

    # primary → NUMBER | STRING | "true" | "false" | "nil" | "(" expression ")" | IDENTIFIER ;
    def primary
      return number_literal if match?(:NUMBER)
      return string_literal if match?(:STRING)
      return bool_literal if match?(:BOOL)
      return nil_literal if match?(:NIL)

      # if match?(:identifier)
      #   return {type: :variable, name: previous.lexeme}
      # end

      # if match?(:left_paren)
      #   expr = expression
      #   consume(:right_paren, "Expect ')' after expression.")
      #   return {type: :grouping, expression: expr}
      # end

      error peek, "Expected expression, got #{peek.inspect}."
    end

    def number_literal
      ::Wid::AST::Expr::NumberLiteral.new(value: previous.value)
    end

    def string_literal
      ::Wid::AST::Expr::StringLiteral.new(value: previous.value)
    end

    def bool_literal
      ::Wid::AST::Expr::BoolLiteral.new(value: previous.value)
    end

    def nil_literal
      ::Wid::AST::Expr::NilLiteral.new(value: nil)
    end

    private

    def match?(*types)
      types.each do |type|
        if check(type)
          advance
          return true
        end
      end

      false
    end

    def check(type)
      return false if eof?

      peek.type == type
    end

    def advance
      @current += 1 unless eof?

      previous
    end

    def eof?
      # peek.type == :EOF
      peek.nil?
    end

    def peek
      @tokens[@current]
    end

    def previous
      @tokens[@current - 1]
    end

    def consume(type, message)
      return advance if check(type)
      raise error(peek, message)
    end

    def error(token, message)
      raise ParseError.new(message)
    end
  end
end
