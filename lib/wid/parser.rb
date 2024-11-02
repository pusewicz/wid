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

      AST::ProgramNode.new(statements: AST::StatementsNode.new(body: statements))
    end

    # Statement → ExpressionStmt | PrintStmt
    def statement
      if match(:PRINT)
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
      expressions << expression while match(:comma)

      # Expect newline or semicolon after print statement
      consume(:"\n", "Expect newline after print statement") unless match(:";")

      AST::PrintNode.new(expressions:)
    end

    # expression → equality ;
    def expression
      equality
    end

    # equality → comparison ( ( "!=" | "==" ) comparison )* ;
    def equality
      receiver = comparison

      while match(:"!=", :"==")
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [comparison])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # comparison → term ( ( ">" | ">=" | "<" | "<=" ) term )* ;
    def comparison
      receiver = term

      while match(:>, :">=", :<, :"<=")
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [term])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # term → factor ( ( "-" | "+" ) factor )* ;
    def term
      receiver = factor

      while match(:-, :+)
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [factor])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # factor → unary ( ( "/" | "*" ) unary )* ;
    def factor
      receiver = unary

      while match(:/, :*)
        name = previous.type
        arguments = AST::ArgumentsNode.new(arguments: [unary])
        receiver = AST::CallNode.new(receiver:, name:, arguments:, block: nil)
      end

      receiver
    end

    # unary → ( "!" | "-" ) unary | primary ;
    def unary
      if match(:!, :-)
        operator = previous.type
        right = unary

        return AST::UnaryNode.new(operator:, right:)
      end

      primary
    end

    # primary → NUMBER | STRING | "true" | "false" | "nil" | "(" expression ")" | IDENTIFIER ;
    def primary
      return number_literal if match(:NUMBER)
      return string_literal if match(:STRING)
      return bool_literal if match(:BOOL)
      return nil_literal if match(:NIL)

      if match(:"(")
        expr = expression
        consume(:")", "Expect ')' after expression.")

        return AST::GroupingNode.new(expression: expr)
      end

      error peek, "Expected a primary expression, got #{peek.inspect}."
    end

    def number_literal
      if previous.value.include?(".")
        ::Wid::AST::FloatNode.new(value: Float(previous.value))
      else
        ::Wid::AST::IntegerNode.new(value: Integer(previous.value))
      end
    end

    def string_literal
      ::Wid::AST::StringNode.new(unescaped: previous.value[1...-1])
    end

    def bool_literal
      if previous.value == "true"
        ::Wid::AST::TrueNode.new
      else
        ::Wid::AST::FalseNode.new
      end
    end

    def nil_literal
      ::Wid::AST::NilNode.new
    end

    private

    def match(*types)
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
